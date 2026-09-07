//! Sloper parameters and records around google-gmail1's users resource methods.

use std::{
    collections::BTreeMap,
    mem,
};

use futures::{
    SinkExt,
    io::AsyncWriteExt,
};
use google_gmail1::{
    api,
    hyper::body::Bytes,
};
use serde::{
    Deserialize,
    Serialize,
    de::DeserializeOwned,
};
use serde_json::Value;
use sloper_extension::{
    Connection,
    Fields,
    Resource,
    Schema,
    Source,
    Writer,
    action,
    operation,
};

use crate::{
    Error,
    client,
};

#[derive(sloper_extension::Connection)]
#[connection(name = "gmail", profile = "google.gmail", scopes = ["https://mail.google.com/"])]
struct Gmail;

#[derive(sloper_extension::Connection)]
#[connection(name = "gmail-settings", profile = "google.gmail", scopes = ["https://www.googleapis.com/auth/gmail.settings.basic"])]
struct Settings;

#[derive(sloper_extension::Connection)]
#[connection(name = "gmail-administration", profile = "google.gmail", scopes = ["https://www.googleapis.com/auth/gmail.settings.basic", "https://www.googleapis.com/auth/gmail.settings.sharing"])]
struct Administration;

#[derive(Deserialize, Serialize, Resource)]
#[resource(name = "gmail-records", key = id)]
struct Record {
    #[schema(max_length = 512)]
    id: String,
    method: String,
    #[schema(max_bytes = 33_554_432, media_types = ["application/json"])]
    data: Source,
    #[schema(max_bytes = 36_700_160, media_types = ["message/rfc822"])]
    raw: Option<Source>,
}

#[derive(Default, Deserialize, Serialize, Schema)]
struct Object {
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Deserialize, Schema)]
struct Parameters<O> {
    operation: O,
    #[serde(default)]
    path: BTreeMap<String, String>,
    #[serde(default)]
    query: Object,
    #[serde(default)]
    body: Object,
    #[schema(max_bytes = 36_700_160, media_types = ["message/rfc822"])]
    raw: Option<Source>,
}

// Each branch consumes its exact path, query, body and source arguments before
// acquiring a token. Generated builders own URL formation and request behavior.
macro_rules! execute {
    ($input:ident, $connection:ident, $hub:ident, $delegate:ident, $call:block) => {{
        $input.finish()?;
        let $hub = client::connect(&$connection).await?;
        let mut $delegate = client::Delegate::default();
        let response = $call;
        let data = $delegate.response(response).await?;
        ($delegate.method(), data)
    }};
}

impl<O> Parameters<O> {
    fn user(&mut self) -> Result<String, Error> {
        self.path.remove("userId").map_or_else(|| Ok("me".into()), nonempty)
    }

    fn path(&mut self, name: &str) -> Result<String, Error> {
        nonempty(
            self.path
                .remove(name)
                .ok_or_else(|| Error::invalid("The selected Gmail operation requires a missing path parameter."))?,
        )
    }

    fn query<T: DeserializeOwned>(&mut self, name: &str) -> Result<Option<T>, Error> {
        self.query
            .fields
            .remove(name)
            .map(|value| {
                validate_query(name, &value)?;
                serde_json::from_value(value).map_err(Error::request)
            })
            .transpose()
    }

    fn body<T: DeserializeOwned + Serialize>(&mut self) -> Result<T, Error> {
        client::request(Value::Object(mem::take(&mut self.body.fields).into_iter().collect()))
    }

    fn media(&mut self) -> Result<Source, Error> {
        self.raw
            .take()
            .ok_or_else(|| Error::invalid("This Gmail operation requires a raw MIME Source."))
    }

    fn finish(&self) -> Result<(), Error> {
        if !self.path.is_empty() || !self.query.fields.is_empty() || !self.body.fields.is_empty() || self.raw.is_some()
        {
            return Err(Error::invalid(
                "A supplied path, query, body, or raw field is not used by the selected Gmail operation.",
            ));
        }
        Ok(())
    }
}

fn nonempty(value: String) -> Result<String, Error> {
    if value.trim().is_empty() {
        return Err(Error::invalid("Gmail path parameters must not be empty."));
    }
    Ok(value)
}

fn validate_query(name: &str, value: &Value) -> Result<(), Error> {
    let valid = match name {
        "maxResults" => value.as_u64().is_some_and(|size| (1..=500).contains(&size)),
        "format" => matches!(value.as_str(), Some("full" | "minimal" | "metadata" | "raw")),
        "internalDateSource" => matches!(value.as_str(), Some("dateHeader" | "receivedTime")),
        "historyTypes" => {
            value.as_array().is_some_and(|types| {
                types.iter().all(|kind| {
                    matches!(
                        kind.as_str(),
                        Some("messageAdded" | "messageDeleted" | "labelAdded" | "labelRemoved")
                    )
                })
            })
        },
        _ => true,
    };
    if !valid {
        return Err(Error::invalid(
            "A Gmail query value is outside its supported range or choices.",
        ));
    }
    Ok(())
}

async fn write(mut output: Writer<Record>, method: &str, bytes: &[u8], raw: Option<Source>) -> Result<(), Error> {
    let mut data = output.source("data", "gmail-response.json").await?;
    data.write_all(bytes).await?;
    let record = Record {
        id: operation()?,
        method: method.into(),
        data: data.close().await?,
        raw,
    };
    output.send(vec![record].into()).await?;
    Ok(())
}
#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum UsersOperation {
    GetProfile,
    Stop,
    Watch,
}

/// Runs a generated Gmail users resource operation.
#[action]
async fn users(
    mut input: Parameters<UsersOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        UsersOperation::GetProfile => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().get_profile(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        UsersOperation::Stop => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().stop(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        UsersOperation::Watch => {
            let request: api::WatchRequest = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().watch(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum MessagesOperation {
    BatchDelete,
    BatchModify,
    Delete,
    Get,
    Import,
    Insert,
    List,
    Modify,
    Send,
    Trash,
    Untrash,
}

/// Runs a generated Gmail messages resource operation.
#[action]
async fn messages(
    mut input: Parameters<MessagesOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        MessagesOperation::Import | MessagesOperation::Insert | MessagesOperation::Send => {
            upload_message(input, &connection, &user).await?
        },
        MessagesOperation::BatchDelete => {
            let request: api::BatchDeleteMessagesRequest = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_batch_delete(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::BatchModify => {
            let request: api::BatchModifyMessagesRequest = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_batch_modify(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::Delete => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_delete(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::Get => {
            let id = input.path("id")?;
            let metadata_headers: Vec<String> = input.query("metadataHeaders")?.unwrap_or_default();
            let format: Option<String> = input.query("format")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().messages_get(&user, &id);
                for value in metadata_headers {
                    call = call.add_metadata_headers(&value);
                }
                if let Some(value) = format {
                    call = call.format(&value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::List => {
            let q: Option<String> = input.query("q")?;
            let page_token: Option<String> = input.query("pageToken")?;
            let max_results: Option<u32> = input.query("maxResults")?;
            let label_ids: Vec<String> = input.query("labelIds")?.unwrap_or_default();
            let include_spam_trash: Option<bool> = input.query("includeSpamTrash")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().messages_list(&user);
                if let Some(value) = q {
                    call = call.q(&value);
                }
                if let Some(value) = page_token {
                    call = call.page_token(&value);
                }
                if let Some(value) = max_results {
                    call = call.max_results(value);
                }
                for value in label_ids {
                    call = call.add_label_ids(&value);
                }
                if let Some(value) = include_spam_trash {
                    call = call.include_spam_trash(value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::Modify => {
            let request: api::ModifyMessageRequest = input.body()?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_modify(request, &user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::Trash => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_trash(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        MessagesOperation::Untrash => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_untrash(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum AttachmentsOperation {
    Get,
}

/// Runs a generated Gmail attachments resource operation.
#[action]
async fn attachments(
    mut input: Parameters<AttachmentsOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        AttachmentsOperation::Get => {
            let message_id = input.path("messageId")?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().messages_attachments_get(&user, &message_id, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum DraftsOperation {
    Create,
    Delete,
    Get,
    List,
    Send,
    Update,
}

/// Runs a generated Gmail drafts resource operation.
#[action]
async fn drafts(
    mut input: Parameters<DraftsOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        DraftsOperation::Create | DraftsOperation::Update | DraftsOperation::Send => {
            upload_draft(input, &connection, &user).await?
        },
        DraftsOperation::Delete => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().drafts_delete(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        DraftsOperation::Get => {
            let id = input.path("id")?;
            let format: Option<String> = input.query("format")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().drafts_get(&user, &id);
                if let Some(value) = format {
                    call = call.format(&value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        DraftsOperation::List => {
            let q: Option<String> = input.query("q")?;
            let page_token: Option<String> = input.query("pageToken")?;
            let max_results: Option<u32> = input.query("maxResults")?;
            let include_spam_trash: Option<bool> = input.query("includeSpamTrash")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().drafts_list(&user);
                if let Some(value) = q {
                    call = call.q(&value);
                }
                if let Some(value) = page_token {
                    call = call.page_token(&value);
                }
                if let Some(value) = max_results {
                    call = call.max_results(value);
                }
                if let Some(value) = include_spam_trash {
                    call = call.include_spam_trash(value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum ThreadsOperation {
    Delete,
    Get,
    List,
    Modify,
    Trash,
    Untrash,
}

/// Runs a generated Gmail threads resource operation.
#[action]
async fn threads(
    mut input: Parameters<ThreadsOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        ThreadsOperation::Delete => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().threads_delete(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        ThreadsOperation::Get => {
            let id = input.path("id")?;
            let metadata_headers: Vec<String> = input.query("metadataHeaders")?.unwrap_or_default();
            let format: Option<String> = input.query("format")?;
            if format.as_deref() == Some("raw") {
                return Err(Error::invalid("Thread format must be full, minimal, or metadata.").into());
            }
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().threads_get(&user, &id);
                for value in metadata_headers {
                    call = call.add_metadata_headers(&value);
                }
                if let Some(value) = format {
                    call = call.format(&value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        ThreadsOperation::List => {
            let q: Option<String> = input.query("q")?;
            let page_token: Option<String> = input.query("pageToken")?;
            let max_results: Option<u32> = input.query("maxResults")?;
            let label_ids: Vec<String> = input.query("labelIds")?.unwrap_or_default();
            let include_spam_trash: Option<bool> = input.query("includeSpamTrash")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().threads_list(&user);
                if let Some(value) = q {
                    call = call.q(&value);
                }
                if let Some(value) = page_token {
                    call = call.page_token(&value);
                }
                if let Some(value) = max_results {
                    call = call.max_results(value);
                }
                for value in label_ids {
                    call = call.add_label_ids(&value);
                }
                if let Some(value) = include_spam_trash {
                    call = call.include_spam_trash(value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        ThreadsOperation::Modify => {
            let request: api::ModifyThreadRequest = input.body()?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().threads_modify(request, &user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        ThreadsOperation::Trash => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().threads_trash(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        ThreadsOperation::Untrash => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().threads_untrash(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum HistoryOperation {
    List,
}

/// Runs a generated Gmail history resource operation.
#[action]
async fn history(
    mut input: Parameters<HistoryOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        HistoryOperation::List => {
            let start_history_id = input
                .query::<String>("startHistoryId")?
                .ok_or_else(|| Error::invalid("Listing Gmail history requires query.startHistoryId."))?
                .parse::<u64>()
                .map_err(Error::from)?;
            let page_token: Option<String> = input.query("pageToken")?;
            let max_results: Option<u32> = input.query("maxResults")?;
            let label_id: Option<String> = input.query("labelId")?;
            let history_types: Vec<String> = input.query("historyTypes")?.unwrap_or_default();
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().history_list(&user);
                call = call.start_history_id(start_history_id);
                if let Some(value) = page_token {
                    call = call.page_token(&value);
                }
                if let Some(value) = max_results {
                    call = call.max_results(value);
                }
                if let Some(value) = label_id {
                    call = call.label_id(&value);
                }
                for value in history_types {
                    call = call.add_history_types(&value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum LabelsOperation {
    Create,
    Delete,
    Get,
    List,
    Patch,
    Update,
}

/// Runs a generated Gmail labels resource operation.
#[action]
async fn labels(
    mut input: Parameters<LabelsOperation>,
    connection: Connection<Gmail>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        LabelsOperation::Create => {
            let request: api::Label = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().labels_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        LabelsOperation::Delete => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().labels_delete(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        LabelsOperation::Get => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().labels_get(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        LabelsOperation::List => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().labels_list(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        LabelsOperation::Patch => {
            let request: api::Label = input.body()?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().labels_patch(request, &user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        LabelsOperation::Update => {
            let request: api::Label = input.body()?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().labels_update(request, &user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum SettingsOperation {
    GetImap,
    GetLanguage,
    GetPop,
    GetVacation,
    UpdateImap,
    UpdateLanguage,
    UpdatePop,
    UpdateVacation,
}

/// Runs a generated Gmail settings resource operation.
#[action]
async fn settings(
    mut input: Parameters<SettingsOperation>,
    connection: Connection<Settings>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        SettingsOperation::GetImap => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_get_imap(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::GetLanguage => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_get_language(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::GetPop => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_get_pop(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::GetVacation => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_get_vacation(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::UpdateImap => {
            let request: api::ImapSettings = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_update_imap(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::UpdateLanguage => {
            let request: api::LanguageSettings = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_update_language(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::UpdatePop => {
            let request: api::PopSettings = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_update_pop(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SettingsOperation::UpdateVacation => {
            let request: api::VacationSettings = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_update_vacation(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum FiltersOperation {
    Create,
    Delete,
    Get,
    List,
}

/// Runs a generated Gmail filters resource operation.
#[action]
async fn filters(
    mut input: Parameters<FiltersOperation>,
    connection: Connection<Settings>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        FiltersOperation::Create => {
            let request: api::Filter = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_filters_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        FiltersOperation::Delete => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_filters_delete(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        FiltersOperation::Get => {
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_filters_get(&user, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        FiltersOperation::List => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_filters_list(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum ForwardingAddressesOperation {
    Create,
    Delete,
    Get,
    List,
}

/// Runs a generated Gmail forwarding addresses resource operation.
#[action]
async fn forwarding_addresses(
    mut input: Parameters<ForwardingAddressesOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        ForwardingAddressesOperation::Create => {
            let request: api::ForwardingAddress = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_forwarding_addresses_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        ForwardingAddressesOperation::Delete => {
            let forwarding_email = input.path("forwardingEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub
                    .users()
                    .settings_forwarding_addresses_delete(&user, &forwarding_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        ForwardingAddressesOperation::Get => {
            let forwarding_email = input.path("forwardingEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_forwarding_addresses_get(&user, &forwarding_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        ForwardingAddressesOperation::List => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_forwarding_addresses_list(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum SendAsOperation {
    Create,
    Delete,
    Get,
    List,
    Patch,
    Update,
    Verify,
}

/// Runs a generated Gmail send as resource operation.
#[action]
async fn send_as(
    mut input: Parameters<SendAsOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        SendAsOperation::Create => {
            let request: api::SendAs = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SendAsOperation::Delete => {
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_delete(&user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        SendAsOperation::Get => {
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_get(&user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        SendAsOperation::List => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_list(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        SendAsOperation::Patch => {
            let request: api::SendAs = input.body()?;
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_patch(request, &user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        SendAsOperation::Update => {
            let request: api::SendAs = input.body()?;
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_update(request, &user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        SendAsOperation::Verify => {
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_verify(&user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum DelegatesOperation {
    Create,
    Delete,
    Get,
    List,
}

/// Runs a generated Gmail delegates resource operation.
#[action]
async fn delegates(
    mut input: Parameters<DelegatesOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        DelegatesOperation::Create => {
            let request: api::Delegate = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_delegates_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        DelegatesOperation::Delete => {
            let delegate_email = input.path("delegateEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_delegates_delete(&user, &delegate_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        DelegatesOperation::Get => {
            let delegate_email = input.path("delegateEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_delegates_get(&user, &delegate_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        DelegatesOperation::List => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_delegates_list(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum SmimeInfoOperation {
    Delete,
    Get,
    Insert,
    List,
    SetDefault,
}

/// Runs a generated Gmail smime info resource operation.
#[action]
async fn smime_info(
    mut input: Parameters<SmimeInfoOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        SmimeInfoOperation::Delete => {
            let send_as_email = input.path("sendAsEmail")?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub
                    .users()
                    .settings_send_as_smime_info_delete(&user, &send_as_email, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        SmimeInfoOperation::Get => {
            let send_as_email = input.path("sendAsEmail")?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_smime_info_get(&user, &send_as_email, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
        SmimeInfoOperation::Insert => {
            let request: api::SmimeInfo = input.body()?;
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub
                    .users()
                    .settings_send_as_smime_info_insert(request, &user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        SmimeInfoOperation::List => {
            let send_as_email = input.path("sendAsEmail")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_send_as_smime_info_list(&user, &send_as_email);
                call.delegate(&mut delegate).doit().await
            })
        },
        SmimeInfoOperation::SetDefault => {
            let send_as_email = input.path("sendAsEmail")?;
            let id = input.path("id")?;
            execute!(input, connection, hub, delegate, {
                let call = hub
                    .users()
                    .settings_send_as_smime_info_set_default(&user, &send_as_email, &id);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum CseIdentitiesOperation {
    Create,
    Delete,
    Get,
    List,
    Patch,
}

/// Runs a generated Gmail cse identities resource operation.
#[action]
async fn cse_identities(
    mut input: Parameters<CseIdentitiesOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        CseIdentitiesOperation::Create => {
            let request: api::CseIdentity = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_identities_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseIdentitiesOperation::Delete => {
            let cse_email_address = input.path("cseEmailAddress")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_identities_delete(&user, &cse_email_address);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseIdentitiesOperation::Get => {
            let cse_email_address = input.path("cseEmailAddress")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_identities_get(&user, &cse_email_address);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseIdentitiesOperation::List => {
            let page_token: Option<String> = input.query("pageToken")?;
            let page_size: Option<i32> = input.query("pageSize")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().settings_cse_identities_list(&user);
                if let Some(value) = page_token {
                    call = call.page_token(&value);
                }
                if let Some(value) = page_size {
                    call = call.page_size(value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        CseIdentitiesOperation::Patch => {
            let request: api::CseIdentity = input.body()?;
            let email_address = input.path("emailAddress")?;
            execute!(input, connection, hub, delegate, {
                let call = hub
                    .users()
                    .settings_cse_identities_patch(request, &user, &email_address);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum CseKeypairsOperation {
    Create,
    Disable,
    Enable,
    Get,
    List,
    Obliterate,
}

/// Runs a generated Gmail cse keypairs resource operation.
#[action]
async fn cse_keypairs(
    mut input: Parameters<CseKeypairsOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        CseKeypairsOperation::Create => {
            let request: api::CseKeyPair = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_keypairs_create(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseKeypairsOperation::Disable => {
            let request: api::DisableCseKeyPairRequest = input.body()?;
            let key_pair_id = input.path("keyPairId")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_keypairs_disable(request, &user, &key_pair_id);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseKeypairsOperation::Enable => {
            let request: api::EnableCseKeyPairRequest = input.body()?;
            let key_pair_id = input.path("keyPairId")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_keypairs_enable(request, &user, &key_pair_id);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseKeypairsOperation::Get => {
            let key_pair_id = input.path("keyPairId")?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_cse_keypairs_get(&user, &key_pair_id);
                call.delegate(&mut delegate).doit().await
            })
        },
        CseKeypairsOperation::List => {
            let page_token: Option<String> = input.query("pageToken")?;
            let page_size: Option<i32> = input.query("pageSize")?;
            execute!(input, connection, hub, delegate, {
                let mut call = hub.users().settings_cse_keypairs_list(&user);
                if let Some(value) = page_token {
                    call = call.page_token(&value);
                }
                if let Some(value) = page_size {
                    call = call.page_size(value);
                }
                call.delegate(&mut delegate).doit().await
            })
        },
        CseKeypairsOperation::Obliterate => {
            let request: api::ObliterateCseKeyPairRequest = input.body()?;
            let key_pair_id = input.path("keyPairId")?;
            execute!(input, connection, hub, delegate, {
                let call = hub
                    .users()
                    .settings_cse_keypairs_obliterate(request, &user, &key_pair_id);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}
async fn upload_message(
    mut input: Parameters<MessagesOperation>,
    connection: &Connection<Gmail>,
    user: &str,
) -> Result<(&'static str, Bytes), Error> {
    let request: api::Message = input.body()?;
    if request.raw.is_some() {
        return Err(Error::invalid("Use the raw Source for MIME content; omit body.raw."));
    }
    let (process, never_spam) = if matches!(input.operation, MessagesOperation::Import) {
        (
            input.query::<bool>("processForCalendar")?,
            input.query::<bool>("neverMarkSpam")?,
        )
    } else {
        (None, None)
    };
    let (date, deleted) = if matches!(input.operation, MessagesOperation::Import | MessagesOperation::Insert) {
        (
            input.query::<String>("internalDateSource")?,
            input.query::<bool>("deleted")?,
        )
    } else {
        (None, None)
    };
    let source = input.media()?;
    input.finish()?;
    let media = client::media(&source).await?;
    let (method, path) = match input.operation {
        MessagesOperation::Import => ("gmail.users.messages.import", vec![user, "messages", "import"]),
        MessagesOperation::Insert => ("gmail.users.messages.insert", vec![user, "messages"]),
        MessagesOperation::Send => ("gmail.users.messages.send", vec![user, "messages", "send"]),
        _ => return Err(Error::invalid("The selected operation does not upload a message.")),
    };
    let query = [
        ("processForCalendar", process.map(|value| value.to_string())),
        ("neverMarkSpam", never_spam.map(|value| value.to_string())),
        ("internalDateSource", date.clone()),
        ("deleted", deleted.map(|value| value.to_string())),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name, value)))
    .collect();
    let target = client::JsonUpload {
        method,
        verb: http::Method::POST,
        path: &path,
        query,
    };
    if let Some(result) = client::collision_upload(connection, &request, media.get_ref(), target).await? {
        return Ok(result);
    }
    let hub = client::connect(connection).await?;
    let mut delegate = client::Delegate::default();
    let mime = "message/rfc822".parse()?;
    let response = match input.operation {
        MessagesOperation::Import => {
            let mut call = hub.users().messages_import(request, user);
            if let Some(value) = process {
                call = call.process_for_calendar(value);
            }
            if let Some(value) = never_spam {
                call = call.never_mark_spam(value);
            }
            if let Some(value) = date {
                call = call.internal_date_source(&value);
            }
            if let Some(value) = deleted {
                call = call.deleted(value);
            }
            call.delegate(&mut delegate).upload(media, mime).await
        },
        MessagesOperation::Insert => {
            let mut call = hub.users().messages_insert(request, user);
            if let Some(value) = date {
                call = call.internal_date_source(&value);
            }
            if let Some(value) = deleted {
                call = call.deleted(value);
            }
            call.delegate(&mut delegate).upload(media, mime).await
        },
        MessagesOperation::Send => {
            hub.users()
                .messages_send(request, user)
                .delegate(&mut delegate)
                .upload(media, mime)
                .await
        },
        _ => return Err(Error::invalid("The selected operation does not upload a message.")),
    };
    Ok((delegate.method(), delegate.response(response).await?))
}

async fn upload_draft(
    mut input: Parameters<DraftsOperation>,
    connection: &Connection<Gmail>,
    user: &str,
) -> Result<(&'static str, Bytes), Error> {
    let request: api::Draft = input.body()?;
    if request
        .message
        .as_ref()
        .and_then(|message| message.raw.as_ref())
        .is_some()
    {
        return Err(Error::invalid(
            "Use the raw Source for MIME content; omit body.message.raw.",
        ));
    }
    let id = if matches!(input.operation, DraftsOperation::Update) {
        input.path("id")?
    } else {
        String::new()
    };
    let source = if matches!(input.operation, DraftsOperation::Send) {
        if request.id.as_ref().is_none_or(|id| id.trim().is_empty()) {
            return Err(Error::invalid("Sending a draft requires body.id."));
        }
        if input.raw.is_none() && request.message.is_some() {
            return Err(Error::invalid("Replacement draft content requires a raw MIME Source."));
        }
        input.raw.take()
    } else {
        Some(input.media()?)
    };
    input.finish()?;
    let Some(source) = source else {
        return client::send_existing_draft(&client::connect(connection).await?, user, request).await;
    };
    let media = client::media(&source).await?;
    let (method, verb, path) = match input.operation {
        DraftsOperation::Create => ("gmail.users.drafts.create", http::Method::POST, vec![user, "drafts"]),
        DraftsOperation::Update => {
            (
                "gmail.users.drafts.update",
                http::Method::PUT,
                vec![user, "drafts", &id],
            )
        },
        DraftsOperation::Send => {
            (
                "gmail.users.drafts.send",
                http::Method::POST,
                vec![user, "drafts", "send"],
            )
        },
        _ => return Err(Error::invalid("The selected operation does not upload a draft.")),
    };
    let target = client::JsonUpload {
        method,
        verb,
        path: &path,
        query: Vec::new(),
    };
    if let Some(result) = client::collision_upload(connection, &request, media.get_ref(), target).await? {
        return Ok(result);
    }
    let hub = client::connect(connection).await?;
    let mut delegate = client::Delegate::default();
    let mime = "message/rfc822".parse()?;
    let data = match input.operation {
        DraftsOperation::Create => {
            let result = hub
                .users()
                .drafts_create(request, user)
                .delegate(&mut delegate)
                .upload(media, mime)
                .await;
            delegate.response(result).await?
        },
        DraftsOperation::Update => {
            let result = hub
                .users()
                .drafts_update(request, user, &id)
                .delegate(&mut delegate)
                .upload(media, mime)
                .await;
            delegate.response(result).await?
        },
        DraftsOperation::Send => {
            let result = hub
                .users()
                .drafts_send(request, user)
                .delegate(&mut delegate)
                .upload(media, mime)
                .await;
            delegate.response(result).await?
        },
        _ => return Err(Error::invalid("The selected operation does not upload a draft.")),
    };
    Ok((delegate.method(), data))
}

#[derive(Clone, Copy, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
enum AutoForwardingOperation {
    Get,
    Update,
}

/// Reads or updates automatic forwarding with Gmail's sharing capability.
#[action]
async fn auto_forwarding(
    mut input: Parameters<AutoForwardingOperation>,
    connection: Connection<Administration>,
    output: Writer<Record>,
) -> sloper_extension::Result<()> {
    let user = input.user()?;
    let raw = input.raw.clone();
    let (method, data) = match input.operation {
        AutoForwardingOperation::Get => {
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_get_auto_forwarding(&user);
                call.delegate(&mut delegate).doit().await
            })
        },
        AutoForwardingOperation::Update => {
            let request: api::AutoForwarding = input.body()?;
            execute!(input, connection, hub, delegate, {
                let call = hub.users().settings_update_auto_forwarding(request, &user);
                call.delegate(&mut delegate).doit().await
            })
        },
    };
    write(output, method, &data, raw).await.map_err(Into::into)
}
