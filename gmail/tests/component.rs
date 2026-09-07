#![cfg(not(target_arch = "wasm32"))] // The canonical host executes WASI components from a native process.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        Mutex,
        OnceLock,
        atomic::{
            AtomicUsize,
            Ordering,
        },
    },
};

use async_trait::async_trait;
use serde_json::{
    Value,
    json,
};
use sloper_extension_host::{
    AccessToken,
    Engine,
    Error as ExtensionHostError,
    Failure,
    Host,
    HostError,
    LogKind,
    LogLevel,
    Page,
    Request,
    Source as HostSource,
    check_component,
};
use tokio::{
    io::AsyncRead,
    sync::{
        mpsc,
        watch,
    },
};
use tokio_wasi as tokio;

const EXPECTED_ACTIONS: [(&str, &[&str]); 16] = [
    ("users", &["getProfile", "stop", "watch"]),
    (
        "messages",
        &[
            "batchDelete",
            "batchModify",
            "delete",
            "get",
            "import",
            "insert",
            "list",
            "modify",
            "send",
            "trash",
            "untrash",
        ],
    ),
    ("attachments", &["get"]),
    ("drafts", &["create", "delete", "get", "list", "send", "update"]),
    ("threads", &["delete", "get", "list", "modify", "trash", "untrash"]),
    ("history", &["list"]),
    ("labels", &["create", "delete", "get", "list", "patch", "update"]),
    (
        "settings",
        &[
            "getImap",
            "getLanguage",
            "getPop",
            "getVacation",
            "updateImap",
            "updateLanguage",
            "updatePop",
            "updateVacation",
        ],
    ),
    ("auto-forwarding", &["get", "update"]),
    ("filters", &["create", "delete", "get", "list"]),
    ("forwarding-addresses", &["create", "delete", "get", "list"]),
    (
        "send-as",
        &["create", "delete", "get", "list", "patch", "update", "verify"],
    ),
    ("delegates", &["create", "delete", "get", "list"]),
    ("smime-info", &["delete", "get", "insert", "list", "setDefault"]),
    ("cse-identities", &["create", "delete", "get", "list", "patch"]),
    (
        "cse-keypairs",
        &["create", "disable", "enable", "get", "list", "obliterate"],
    ),
];

static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();

#[derive(Default)]
struct ProviderGate {
    connections: Mutex<Vec<String>>,
    source_calls: AtomicUsize,
    write_calls: AtomicUsize,
    other_calls: AtomicUsize,
}

#[async_trait]
impl Host for ProviderGate {
    async fn access_token(&self, connection: &str) -> Result<AccessToken, HostError> {
        self.connections
            .lock()
            .expect("connection observations are not poisoned")
            .push(connection.into());
        // No account credentials are issued, so these real guest actions
        // cannot reach Gmail or perform provider mutations.
        Err(HostError::unauthorized())
    }

    async fn open_source(&self, _: &str) -> Result<Pin<Box<dyn AsyncRead + Send>>, HostError> {
        self.source_calls.fetch_add(1, Ordering::AcqRel);
        Err(HostError::invalid("the provider gate lends no sources"))
    }

    async fn read(&self, _: &str) -> Result<Option<Page>, HostError> {
        self.other_calls.fetch_add(1, Ordering::AcqRel);
        Err(HostError::invalid("Gmail declares no resource readers"))
    }

    async fn write(&self, _: &str, _: Option<&str>, _: mpsc::Receiver<String>) -> Result<(), HostError> {
        self.write_calls.fetch_add(1, Ordering::AcqRel);
        Err(HostError::invalid("a rejected provider exchange cannot write records"))
    }

    async fn checkpoint(&self, _: &str, _: Option<&str>, _: &str) -> Result<(), HostError> {
        self.other_calls.fetch_add(1, Ordering::AcqRel);
        Err(HostError::invalid("a rejected provider exchange cannot checkpoint"))
    }

    async fn source(&self, _: &str, _: &str, _: &str, _: Pin<Box<dyn AsyncRead + Send>>) -> Result<String, HostError> {
        self.source_calls.fetch_add(1, Ordering::AcqRel);
        Err(HostError::invalid(
            "a rejected provider exchange cannot create output sources",
        ))
    }

    fn log(&self, _: LogLevel, _: &str, _: LogKind) {}

    async fn finish(&self) -> Result<(), HostError> {
        self.other_calls.fetch_add(1, Ordering::AcqRel);
        Err(HostError::invalid("a failed action cannot settle successfully"))
    }
}

impl ProviderGate {
    fn assert_no_effects(&self) {
        assert!(
            self.connections
                .lock()
                .expect("connection observations are not poisoned")
                .is_empty(),
            "invalid parameters must not request credentials"
        );
        self.assert_no_resource_calls();
    }

    fn assert_no_resource_calls(&self) {
        assert_eq!(self.source_calls.load(Ordering::Acquire), 0);
        assert_eq!(self.write_calls.load(Ordering::Acquire), 0);
        assert_eq!(self.other_calls.load(Ordering::Acquire), 0);
    }
}

fn component() -> &'static [u8] {
    COMPONENT.get_or_init(|| {
        let artifact = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/extension.wasm");
        fs::read(artifact).expect("run `sloper-extension build gmail` before testing the distributable component")
    })
}

fn request(action: &str, parameters: &Value) -> Request {
    Request {
        operation: "gmail-component-test".into(),
        action: action.into(),
        parameters: parameters.to_string(),
        configuration: "{}".into(),
        sources: parameters["raw"]
            .as_str()
            .map(|id| {
                HostSource {
                    id: id.into(),
                    filename: "message.eml".into(),
                    media_type: "message/rfc822".into(),
                    // Metadata grants admission; the provider gate never opens bytes.
                    size: 1,
                }
            })
            .into_iter()
            .collect(),
        cursors: vec![],
    }
}

fn invalid_semantic_parameters() -> [(&'static str, Value); 31] {
    [
        ("users", json!({"operation":"getProfile","path":{"userId":" "}})),
        ("users", json!({"operation":"getProfile","path":{"id":"unused"}})),
        ("users", json!({"operation":"stop","query":{"pageToken":"unused"}})),
        ("users", json!({"operation":"getProfile","body":{"topicName":"unused"}})),
        ("users", json!({"operation":"getProfile","raw":"source:message"})),
        ("users", json!({"operation":"watch","body":{"unknownField":true}})),
        ("messages", json!({"operation":"get"})),
        ("messages", json!({"operation":"get","path":{"id":""}})),
        ("messages", json!({"operation":"list","query":{"maxResults":"10"}})),
        ("messages", json!({"operation":"list","query":{"maxResults":-1}})),
        (
            "messages",
            json!({"operation":"list","query":{"includeSpamTrash":"true"}}),
        ),
        ("messages", json!({"operation":"list","query":{"labelIds":[1]}})),
        ("messages", json!({"operation":"list","query":{"unknownField":true}})),
        (
            "messages",
            json!({"operation":"modify","path":{"id":"message"},"body":{"addClassificationLabels":[]}}),
        ),
        ("messages", json!({"operation":"send"})),
        (
            "messages",
            json!({"operation":"send","body":{"raw":"Zg"},"raw":"source:message"}),
        ),
        ("messages", json!({"operation":"import"})),
        ("messages", json!({"operation":"insert"})),
        ("attachments", json!({"operation":"get","path":{"messageId":"message"}})),
        ("drafts", json!({"operation":"create"})),
        (
            "drafts",
            json!({"operation":"create","body":{"message":{"raw":"Zg"}},"raw":"source:message"}),
        ),
        ("drafts", json!({"operation":"update","path":{"id":"draft"}})),
        ("history", json!({"operation":"list"})),
        (
            "history",
            json!({"operation":"list","query":{"startHistoryId":"not-a-number"}}),
        ),
        (
            "send-as",
            json!({"operation":"patch","path":{"sendAsEmail":"sender@example.com"},"body":{"signature":null}}),
        ),
        (
            "delegates",
            json!({"operation":"list","path":{"delegateEmail":"delegate@example.com"}}),
        ),
        ("delegates", json!({"operation":"delete"})),
        (
            "smime-info",
            json!({"operation":"get","path":{"sendAsEmail":"sender@example.com"}}),
        ),
        (
            "cse-identities",
            json!({"operation":"get","path":{"cseEmailAddress":"sender@example.com"},"query":{"pageSize":20}}),
        ),
        ("cse-keypairs", json!({"operation":"disable"})),
        (
            "cse-keypairs",
            json!({"operation":"get","path":{"keyPairId":"key"},"query":{"pageSize":20}}),
        ),
    ]
}

#[test]
fn component_manifest_declares_current_actions_resources_and_scoped_connections() {
    let bytes = component();
    let manifest = check_component(bytes).expect("current Gmail component has a valid manifest");
    assert_eq!(manifest.name, "sloper.gmail");
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));

    assert_eq!(manifest.actions.len(), 16);
    assert_eq!(
        manifest.actions.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        EXPECTED_ACTIONS
            .iter()
            .map(|(action, _)| *action)
            .collect::<BTreeSet<_>>()
    );
    let mut operation_count = 0;
    for (action, expected_operations) in EXPECTED_ACTIONS {
        let parameters = manifest.actions[action]
            .parameters
            .as_ref()
            .expect("every resource action declares parameters")
            .as_value();
        let properties = parameters["properties"].as_object().expect("parameters are an object");
        assert_eq!(
            properties.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            ["operation", "path", "query", "body", "raw"]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "action={action}"
        );
        assert_eq!(parameters["required"], json!(["operation"]), "action={action}");
        assert_eq!(parameters["additionalProperties"], false, "action={action}");
        let operations = properties["operation"]["enum"]
            .as_array()
            .expect("each resource operation is a closed enum");
        assert_eq!(
            operations
                .iter()
                .map(|operation| operation.as_str().expect("operations are named strings"))
                .collect::<BTreeSet<_>>(),
            expected_operations.iter().copied().collect::<BTreeSet<_>>(),
            "action={action}"
        );
        operation_count += operations.len();
        assert_eq!(properties["raw"]["type"], json!(["string", "null"]), "action={action}");
        assert_eq!(properties["raw"]["format"], "source", "action={action}");
        assert_eq!(properties["raw"]["maxBytes"], 36_700_160, "action={action}");
    }
    assert_eq!(operation_count, 79);
    assert_eq!(manifest.resources.len(), 1);
    let records = &manifest.resources["gmail-records"];
    assert_eq!(records.key.as_deref(), Some("id"));
    let record = records.schema.as_value();
    assert_eq!(record["properties"]["id"]["maxLength"], 512);
    assert_eq!(record["properties"]["method"]["type"], "string");
    let data = &record["properties"]["data"];
    assert_eq!(data["type"], "string");
    assert_eq!(data["format"], "source");
    assert_eq!(data["maxBytes"], 33_554_432);
    assert_eq!(data["mediaTypes"], json!(["application/json"]));
    let raw = &record["properties"]["raw"];
    assert_eq!(raw["type"], json!(["string", "null"]));
    assert_eq!(raw["format"], "source");
    assert_eq!(raw["maxBytes"], 36_700_160);
    assert_eq!(raw["mediaTypes"], json!(["message/rfc822"]));
    assert!(
        !record["required"]
            .as_array()
            .expect("record requirements are declared")
            .contains(&json!("raw"))
    );
    assert_eq!(manifest.connections.len(), 3);
    for (name, scopes) in [
        ("gmail", vec!["https://mail.google.com/"]),
        (
            "gmail-settings",
            vec!["https://www.googleapis.com/auth/gmail.settings.basic"],
        ),
        (
            "gmail-administration",
            vec![
                "https://www.googleapis.com/auth/gmail.settings.basic",
                "https://www.googleapis.com/auth/gmail.settings.sharing",
            ],
        ),
    ] {
        let connection = &manifest.connections[name];
        assert_eq!(connection.profile, "google.gmail", "connection={name}");
        assert_eq!(
            connection.scopes.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            scopes.into_iter().collect::<BTreeSet<_>>(),
            "connection={name}"
        );
    }
}

#[tokio::test]
async fn distributable_component_is_admitted_by_the_canonical_host() {
    let bytes = component();
    Engine::new()
        .expect("the canonical host engine config is valid")
        .admit_component(bytes)
        .await
        .expect("the distributable Gmail component instantiates in the canonical host");
}

#[tokio::test]
async fn representative_actions_request_only_their_declared_connection() {
    let bytes = component();
    let manifest = check_component(bytes).expect("current Gmail component has a valid manifest");
    let engine = Engine::new().expect("the canonical host engine config is valid");
    for (action, parameters, connection) in [
        ("users", json!({"operation":"getProfile"}), "gmail"),
        (
            "messages",
            json!({"operation":"list","path":{"userId":"me"},"query":{"maxResults":10,"labelIds":["INBOX"]},"body":{}}),
            "gmail",
        ),
        (
            "attachments",
            json!({"operation":"get","path":{"messageId":"message","id":"attachment"}}),
            "gmail",
        ),
        ("drafts", json!({"operation":"send","body":{"id":"draft"}}), "gmail"),
        ("threads", json!({"operation":"get","path":{"id":"thread"}}), "gmail"),
        (
            "history",
            json!({"operation":"list","query":{"startHistoryId":"123"}}),
            "gmail",
        ),
        ("labels", json!({"operation":"list"}), "gmail"),
        ("settings", json!({"operation":"getLanguage"}), "gmail-settings"),
        (
            "auto-forwarding",
            json!({"operation":"update","body":{"enabled":true,"emailAddress":"recipient@example.com","disposition":"leaveInInbox"}}),
            "gmail-administration",
        ),
        ("filters", json!({"operation":"list"}), "gmail-settings"),
        (
            "forwarding-addresses",
            json!({"operation":"create","body":{"forwardingEmail":"recipient@example.com"}}),
            "gmail-administration",
        ),
        ("send-as", json!({"operation":"list"}), "gmail-administration"),
        ("delegates", json!({"operation":"list"}), "gmail-administration"),
        (
            "smime-info",
            json!({"operation":"list","path":{"sendAsEmail":"sender@example.com"}}),
            "gmail-administration",
        ),
        (
            "cse-identities",
            json!({"operation":"list","query":{"pageSize":10}}),
            "gmail-administration",
        ),
        (
            "cse-keypairs",
            json!({"operation":"disable","path":{"keyPairId":"key"}}),
            "gmail-administration",
        ),
    ] {
        let host = Arc::new(ProviderGate::default());
        let (_stopping, stopping) = watch::channel(None);
        let (_deadline, deadline) = watch::channel(None);
        let result = engine
            .run(
                bytes,
                &manifest,
                request(action, &parameters),
                host.clone(),
                stopping,
                deadline,
            )
            .await
            .expect("a valid action reaches the explicit provider gate");
        assert!(
            matches!(result, Err(Failure::NotConnected(_))),
            "action={action}, result={result:?}"
        );
        assert_eq!(
            host.connections
                .lock()
                .expect("connection observations are not poisoned")
                .as_slice(),
            [connection],
            "action={action}"
        );
        assert_eq!(manifest.actions[action].connections, [connection], "action={action}");
        host.assert_no_resource_calls();
    }
}

#[tokio::test]
async fn semantic_parameter_errors_fail_before_credentials_or_resources() {
    let bytes = component();
    let manifest = check_component(bytes).expect("current Gmail component has a valid manifest");
    let engine = Engine::new().expect("the canonical host engine config is valid");
    for (action, parameters) in invalid_semantic_parameters() {
        let host = Arc::new(ProviderGate::default());
        let (_stopping, stopping) = watch::channel(None);
        let (_deadline, deadline) = watch::channel(None);
        let result = engine
            .run(
                bytes,
                &manifest,
                request(action, &parameters),
                host.clone(),
                stopping,
                deadline,
            )
            .await
            .expect("well-typed parameters reach the action's semantic validation");
        assert!(
            matches!(result, Err(Failure::InvalidParameters(_))),
            "action={action}, parameters={parameters}, result={result:?}"
        );
        host.assert_no_effects();
    }
}

#[tokio::test]
async fn undeclared_actions_and_schema_errors_fail_before_guest_effects() {
    let bytes = component();
    let manifest = check_component(bytes).expect("current Gmail component has a valid manifest");
    let engine = Engine::new().expect("the canonical host engine config is valid");
    for (action, parameters) in [
        ("send-email", json!({})),
        ("users", json!({})),
        ("users", json!({"operation":"getProfile","unexpected":true})),
        ("users", json!({"operation":12})),
        ("users", json!({"operation":"list"})),
        ("settings", json!({"operation":"updateAutoForwarding"})),
        ("users", json!({"operation":"getProfile","path":[]})),
        ("messages", json!({"operation":"get","path":{"id":12}})),
        ("messages", json!({"operation":"list","query":[]})),
        ("messages", json!({"operation":"send","body":[]})),
        (
            "messages",
            json!({"operation":"send","raw":{"source":"not-a-direct-source"}}),
        ),
        ("messages", json!({"operation":"delete","query":null})),
        (
            "cse-keypairs",
            json!({"operation":"unknown","path":{"keyPairId":"key"}}),
        ),
    ] {
        let host = Arc::new(ProviderGate::default());
        let (_stopping, stopping) = watch::channel(None);
        let (_deadline, deadline) = watch::channel(None);
        let result = engine
            .run(
                bytes,
                &manifest,
                request(action, &parameters),
                host.clone(),
                stopping,
                deadline,
            )
            .await;
        assert!(
            matches!(result, Err(ExtensionHostError::Invalid(_))),
            "action={action}, parameters={parameters}, result={result:?}"
        );
        host.assert_no_effects();
    }
}
