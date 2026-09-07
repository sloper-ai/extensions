//! Configuration and response handling for the generated Gmail hub.

use std::{
    future::{
        Ready,
        ready,
    },
    io::{
        self,
        Cursor,
    },
    net::{
        SocketAddr,
        ToSocketAddrs,
    },
    task::{
        Context,
        Poll,
    },
    time::{
        Duration,
        SystemTime,
    },
    vec::IntoIter,
};

use base64::{
    display::Base64Display,
    engine::general_purpose::URL_SAFE_NO_PAD,
};
use futures::io::{
    AsyncRead,
    AsyncReadExt,
};
use google_gmail1::{
    Gmail,
    api,
    common,
    hyper::body::Bytes,
    hyper_util::{
        client::legacy::{
            Client,
            connect::{
                HttpConnector,
                dns::Name,
            },
        },
        rt::TokioExecutor,
    },
};
use http::{
    Method,
    Request,
    StatusCode,
    header::{
        AUTHORIZATION,
        CONTENT_TYPE,
        RETRY_AFTER,
    },
};
use http_body_util::BodyExt;
use hyper_rustls::{
    HttpsConnector,
    HttpsConnectorBuilder,
};
use serde::{
    Serialize,
    Serializer,
    de::DeserializeOwned,
};
use serde_json::{
    Map,
    Value,
};
use sloper_extension::{
    Connection,
    ConnectionType,
    Source,
};
use time::OffsetDateTime;
use tower_service::Service;
use url::form_urlencoded::{
    Serializer as QuerySerializer,
    byte_serialize,
};

use crate::Error;

// The generated Gmail upload builders enforce Google's 35 MiB media ceiling.
pub(super) const MAX_MEDIA_BYTES: u64 = 36_700_160;
// Output data is a declared Source rather than an inline resource item.
pub(super) const MAX_RESPONSE_BYTES: usize = 33_554_432;

pub(super) type Hub = Gmail<HttpsConnector<HttpConnector<Resolver>>>;

/// WASI resolves synchronously through the host; Hyper's resolver needs
/// threads.
#[derive(Clone, Debug)]
pub(super) struct Resolver;

#[derive(Default)]
pub(super) struct Delegate {
    method: &'static str,
    failure: Option<(StatusCode, Option<OffsetDateTime>, bool)>,
}

pub(super) trait ApiResponse {
    fn into_response(self) -> common::Response;
    fn validate_recovered(value: Value) -> Result<(), Error>;
}

impl Service<Name> for Resolver {
    type Error = io::Error;
    type Future = Ready<io::Result<Self::Response>>;
    type Response = IntoIter<SocketAddr>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        ready(
            (name.as_str(), 0)
                .to_socket_addrs()
                .map(|addresses| addresses.collect::<Vec<_>>().into_iter()),
        )
    }
}

impl common::Delegate for Delegate {
    fn begin(&mut self, info: common::MethodInfo) {
        self.method = info.id;
    }

    fn http_failure(&mut self, response: &common::Response, error: Option<&Value>) -> common::Retry {
        self.failure = Some((response.status(), retry_after(response), rate_limited(error)));
        common::Retry::Abort
    }
}

impl Delegate {
    pub(super) fn method(&self) -> &'static str {
        self.method
    }

    pub(super) async fn response<R: ApiResponse>(&self, result: common::Result<R>) -> Result<Bytes, Error> {
        match result {
            Ok(value) => response_bytes(value.into_response()).await,
            // The client rejects unpadded Gmail base64. Validate its generated
            // model after padding, but return the untouched provider document.
            Err(common::Error::JsonDecodeError(original, source)) => {
                if original.len() > MAX_RESPONSE_BYTES {
                    return Err(Error::too_large());
                }
                let mut value: Value = match serde_json::from_str(&original) {
                    Ok(value) => value,
                    Err(_) => return Err(Error::google(common::Error::JsonDecodeError(original, source))),
                };
                pad_byte_fields(&mut value);
                R::validate_recovered(value)?;
                Ok(Bytes::from(original))
            },
            Err(common::Error::Failure(response)) => response_bytes(response).await,
            Err(error) => {
                match self.failure {
                    Some((_, not_before, true)) => Err(Error::rate_limited(not_before)),
                    Some((status, not_before, false)) => Err(Error::status(status, not_before)),
                    None => Err(Error::google(error)),
                }
            },
        }
    }
}

impl ApiResponse for common::Response {
    fn into_response(self) -> common::Response {
        self
    }

    fn validate_recovered(_: Value) -> Result<(), Error> {
        Err(Error::invalid("Gmail returned data for an empty response."))
    }
}

impl<T: DeserializeOwned> ApiResponse for (common::Response, T) {
    fn into_response(self) -> common::Response {
        self.0
    }

    fn validate_recovered(value: Value) -> Result<(), Error> {
        serde_json::from_value::<T>(value).map(|_| ()).map_err(Error::response)
    }
}

pub(super) async fn connect<T: ConnectionType>(connection: &Connection<T>) -> Result<Hub, Error> {
    let token = connection.access_token().await?;
    Ok(new_hub(token.expose_secret().to_owned()))
}

fn new_hub(token: String) -> Hub {
    let mut http = HttpConnector::new_with_resolver(Resolver);
    http.enforce_http(false);
    http.set_connect_timeout(Some(Duration::from_secs(30)));
    let connector = HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only()
        .enable_http2()
        .wrap_connector(http);
    Gmail::new(Client::builder(TokioExecutor::new()).build(connector), token)
}

pub(super) async fn media(source: &Source) -> Result<Cursor<Vec<u8>>, Error> {
    if source.size().is_some_and(|size| size > MAX_MEDIA_BYTES) {
        return Err(Error::too_large());
    }
    read_media(source.open().await?, source.size()).await
}

async fn read_media<R: AsyncRead + Unpin>(reader: R, declared: Option<u64>) -> Result<Cursor<Vec<u8>>, Error> {
    // futures-util 0.3.34 read_to_end reserves 32 bytes before each read,
    // including EOF. Reserve that spare space once for known-size Sources.
    const EOF_RESERVE: usize = 32;
    let limit = declared.unwrap_or(MAX_MEDIA_BYTES);
    if limit > MAX_MEDIA_BYTES {
        return Err(Error::too_large());
    }
    let capacity = match declared {
        Some(size) => {
            usize::try_from(size)
                .ok()
                .and_then(|size| size.checked_add(EOF_RESERVE))
                .ok_or_else(Error::too_large)?
        },
        None => 0,
    };
    let mut bytes = Vec::with_capacity(capacity);
    reader.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > MAX_MEDIA_BYTES {
        return Err(Error::too_large());
    }
    if declared.is_some_and(|length| length != bytes.len() as u64) {
        return Err(Error::invalid("Raw MIME Source length differs from its declared size."));
    }
    if bytes.is_empty() {
        return Err(Error::invalid("Raw MIME content must not be empty."));
    }
    Ok(Cursor::new(bytes))
}

pub(super) fn request<T: DeserializeOwned + Serialize>(mut value: Value) -> Result<T, Error> {
    pad_byte_fields(&mut value);
    let request = T::deserialize(&value).map_err(Error::request)?;
    let canonical = serde_json::to_value(&request)?;
    check_request_fields(&value, &canonical)?;
    Ok(request)
}

fn check_request_fields(input: &Value, canonical: &Value) -> Result<(), Error> {
    match (input, canonical) {
        (Value::Null, _) => {
            Err(Error::invalid(
                "Explicit null is removed by this generated Gmail client; omit the field or use its documented empty \
                 value.",
            ))
        },
        (Value::Object(input), Value::Object(canonical)) => {
            for (key, value) in input {
                let Some(target) = canonical.get(key) else {
                    return Err(Error::invalid(
                        "Body contains a field absent from the generated Gmail request type.",
                    ));
                };
                check_request_fields(value, target)?;
            }
            Ok(())
        },
        (Value::Array(input), Value::Array(canonical)) => {
            for (value, target) in input.iter().zip(canonical) {
                check_request_fields(value, target)?;
            }
            Ok(())
        },
        _ => Ok(()),
    }
}

fn pad_byte_fields(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(name.as_str(), "raw" | "data" | "pkcs12") {
                    if let Value::String(encoded) = value {
                        while encoded.len() % 4 != 0 {
                            encoded.push('=');
                        }
                    }
                } else {
                    pad_byte_fields(value);
                }
            }
        },
        Value::Array(items) => {
            for item in items {
                pad_byte_fields(item);
            }
        },
        _ => {},
    }
}

async fn response_bytes(response: common::Response) -> Result<Bytes, Error> {
    let status = response.status();
    let not_before = retry_after(&response);
    let mut body = response.into_body();
    let mut first = Bytes::new();
    let mut joined: Option<Vec<u8>> = None;
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            let length = joined.as_ref().map_or(first.len(), Vec::len);
            if data.len() > MAX_RESPONSE_BYTES.saturating_sub(length) {
                return Err(Error::too_large());
            }
            if data.is_empty() {
                continue;
            }
            if let Some(joined) = joined.as_mut() {
                joined.extend_from_slice(&data);
            } else if first.is_empty() {
                first = data;
            } else {
                let mut bytes = Vec::with_capacity(first.len() + data.len());
                bytes.extend_from_slice(&first);
                bytes.extend_from_slice(&data);
                first = Bytes::new();
                joined = Some(bytes);
            }
        }
    }
    let bytes = joined.map_or(first, Bytes::from);
    if !status.is_success() {
        let error = serde_json::from_slice::<Value>(&bytes).ok();
        return Err(if rate_limited(error.as_ref()) {
            Error::rate_limited(not_before)
        } else {
            Error::status(status, not_before)
        });
    }
    if bytes.is_empty() {
        return Ok(Bytes::from_static(br#"{"completed":true}"#));
    }
    serde_json::from_slice::<Value>(&bytes).map_err(Error::response)?;
    Ok(bytes)
}

fn rate_limited(error: Option<&Value>) -> bool {
    error
        .and_then(|value| value.pointer("/error/errors"))
        .and_then(Value::as_array)
        .is_some_and(|errors| {
            errors.iter().any(|error| {
                matches!(
                    error.get("reason").and_then(Value::as_str),
                    Some("rateLimitExceeded" | "userRateLimitExceeded")
                )
            })
        })
}

fn retry_after(response: &common::Response) -> Option<OffsetDateTime> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return SystemTime::now()
            .checked_add(Duration::from_secs(seconds))
            .map(OffsetDateTime::from);
    }
    httpdate::parse_http_date(value).ok().map(OffsetDateTime::from)
}

/// Only upload-only operations use this descriptor; ordinary calls remain
/// generated builders. It also names the method on its output record.
pub(super) struct JsonUpload<'a> {
    pub(super) method: &'static str,
    pub(super) verb: Method,
    pub(super) path: &'a [&'a str],
    pub(super) query: Vec<(&'static str, String)>,
}

#[derive(Serialize)]
struct RawMessage<'a> {
    #[serde(flatten)]
    metadata: &'a Map<String, Value>,
    #[serde(serialize_with = "serialize_bytes")]
    raw: &'a [u8],
}

#[derive(Serialize)]
struct RawDraft<'a> {
    #[serde(flatten)]
    metadata: &'a Map<String, Value>,
    message: RawMessage<'a>,
}

fn serialize_bytes<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&Base64Display::new(bytes, &URL_SAFE_NO_PAD))
}

fn upload_body<T: Serialize>(request: &T, media: Option<&[u8]>, draft: bool) -> Result<Vec<u8>, Error> {
    // Serialize metadata before attaching media. The base64 formatter writes
    // straight into the final JSON buffer, with no encoded String/Value copies.
    let mut metadata = serde_json::to_value(request)?;
    common::remove_json_null_values(&mut metadata);
    let Some(media) = media else {
        return Ok(serde_json::to_vec(&metadata)?);
    };
    let Value::Object(mut metadata) = metadata else {
        return Err(Error::invalid("Gmail upload metadata must be an object."));
    };
    if draft {
        let message = metadata.remove("message").unwrap_or_else(|| Value::Object(Map::new()));
        let Value::Object(message) = message else {
            return Err(Error::invalid("Draft message metadata must be an object."));
        };
        let empty = RawDraft {
            metadata: &metadata,
            message: RawMessage {
                metadata: &message,
                raw: &[],
            },
        };
        let full = RawDraft {
            metadata: &metadata,
            message: RawMessage {
                metadata: &message,
                raw: media,
            },
        };
        write_upload(&empty, &full, media.len())
    } else {
        let empty = RawMessage {
            metadata: &metadata,
            raw: &[],
        };
        let full = RawMessage {
            metadata: &metadata,
            raw: media,
        };
        write_upload(&empty, &full, media.len())
    }
}

#[derive(Default)]
struct JsonLength(usize);

impl io::Write for JsonLength {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_upload<T: Serialize>(empty: &T, full: &T, media_length: usize) -> Result<Vec<u8>, Error> {
    let mut length = JsonLength::default();
    serde_json::to_writer(&mut length, empty)?;
    let capacity = base64::encoded_len(media_length, false)
        .and_then(|encoded| length.0.checked_add(encoded))
        .ok_or_else(Error::too_large)?;
    let mut bytes = Vec::with_capacity(capacity);
    serde_json::to_writer(&mut bytes, full)?;
    Ok(bytes)
}

pub(super) async fn collision_upload<C: ConnectionType, T: Serialize>(
    connection: &Connection<C>,
    request: &T,
    media: &[u8],
    target: JsonUpload<'_>,
) -> Result<Option<(&'static str, Bytes)>, Error> {
    // google-apis-common8 hardcodes this delimiter for every multipart body.
    const BOUNDARY: &[u8] = b"MDuXWGyeE33QFXGchb2VFWc4Z7945d";
    if !media.windows(BOUNDARY.len()).any(|window| window == BOUNDARY) {
        return Ok(None);
    }
    let body = upload_body(request, Some(media), target.path.get(1) == Some(&"drafts"))?;
    let hub = connect(connection).await?;
    let bytes = json_upload(&hub, &target, body).await?;
    Ok(Some((target.method, bytes)))
}

async fn json_upload(hub: &Hub, target: &JsonUpload<'_>, body: Vec<u8>) -> Result<Bytes, Error> {
    let path = target
        .path
        .iter()
        .map(|segment| byte_serialize(segment.as_bytes()).collect::<String>())
        .collect::<Vec<_>>()
        .join("/");
    let query = QuerySerializer::new(String::new())
        .extend_pairs(target.query.iter().map(|(key, value)| (*key, value)))
        .finish();
    let uri = format!("https://gmail.googleapis.com/gmail/v1/users/{path}?{query}");
    let request = json_request(hub, target.verb.clone(), &uri, body).await?;
    let response = hub.client.request(request).await?;
    let (parts, body) = response.into_parts();
    response_bytes(common::Response::from_parts(parts, body.boxed())).await
}

async fn json_request(hub: &Hub, method: Method, uri: &str, body: Vec<u8>) -> Result<Request<common::Body>, Error> {
    let token = hub
        .auth
        .get_token(&[])
        .await
        .map_err(|source| Error::google(common::Error::MissingToken(source)))?
        .ok_or_else(|| Error::invalid("The connected Gmail account did not provide a token."))?;
    Ok(Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(CONTENT_TYPE, "application/json")
        .body(common::to_body(body.into()))?)
}

/// Sending a saved draft without replacement MIME needs the JSON-only request
/// form absent from this client's public upload builder.
pub(super) async fn send_existing_draft(
    hub: &Hub,
    user: &str,
    draft: api::Draft,
) -> Result<(&'static str, Bytes), Error> {
    let target = JsonUpload {
        method: "gmail.users.drafts.send",
        verb: Method::POST,
        path: &[user, "drafts", "send"],
        query: Vec::new(),
    };
    let bytes = json_upload(hub, &target, upload_body(&draft, None, true)?).await?;
    Ok((target.method, bytes))
}

// Native tests own the local HTTP simulator; WASI is exercised by component
// tests.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use base64::Engine as _;
    use futures::{
        io::Cursor as AsyncCursor,
        stream,
    };
    use google_gmail1::hyper::{
        Error as HyperError,
        body::Frame,
    };
    use http_body_util::StreamBody;
    use serde_json::json;
    use tokio::{
        io::{
            AsyncReadExt as _,
            AsyncWriteExt as _,
        },
        net::TcpListener,
        task::JoinHandle,
    };
    use tokio_wasi as tokio;

    use super::*;

    async fn mock_server(status: &str, response: &str) -> (Hub, String, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://localhost:{}/", listener.local_addr().unwrap().port());
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: \
             close\r\n\r\n{response}",
            response.len()
        );
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "client must complete its HTTP request");
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8(bytes[..end].to_vec()).unwrap();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket.write_all(reply.as_bytes()).await.unwrap();
            bytes
        });
        let mut http = HttpConnector::new_with_resolver(Resolver);
        http.enforce_http(false);
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http);
        let mut hub = Gmail::new(
            Client::builder(TokioExecutor::new()).build(connector),
            "test-only-token".to_owned(),
        );
        hub.base_url(address.clone());
        hub.root_url(address.clone());
        (hub, address, task)
    }

    #[test]
    fn generated_request_validation_rejects_unknown_fields_and_null_loss() {
        assert!(request::<api::ModifyMessageRequest>(json!({"addClassificationLabels":[]})).is_err());
        assert!(request::<api::SendAs>(json!({"signature":null})).is_err());
        let request = request::<api::ModifyMessageRequest>(json!({"addLabelIds":["STARRED"]})).unwrap();
        assert_eq!(request.add_label_ids, Some(vec!["STARRED".into()]));
    }

    #[test]
    fn generated_byte_requests_accept_gmails_unpadded_encoding() {
        let request = request::<api::SmimeInfo>(json!({"pkcs12":"Zg"})).unwrap();
        assert_eq!(request.pkcs12, Some(b"f".to_vec()));
    }

    #[tokio::test]
    async fn response_recovery_preserves_unpadded_bytes_and_unknown_fields() {
        let original = r#"{"raw":"Zg","futureField":{"value":1}}"#;
        let source = serde_json::from_str::<api::Message>(original).unwrap_err();
        let result: common::Result<(common::Response, api::Message)> =
            Err(common::Error::JsonDecodeError(original.into(), source));
        let bytes = Delegate::default().response(result).await.unwrap();
        assert_eq!(bytes, original.as_bytes());
    }

    #[tokio::test]
    async fn malformed_provider_data_is_not_mistaken_for_padding_failure() {
        let original = r#"{"historyId":"not-an-integer"}"#;
        let source = serde_json::from_str::<api::Message>(original).unwrap_err();
        let result: common::Result<(common::Response, api::Message)> =
            Err(common::Error::JsonDecodeError(original.into(), source));
        assert!(Delegate::default().response(result).await.is_err());
    }

    #[tokio::test]
    async fn generated_multipart_exchange_retains_mime_and_original_response() {
        let expected = r#"{"id":"sent","futureField":42}"#;
        let (hub, _, server) = mock_server("200 OK", expected).await;
        let raw = b"From: sender@example.com\r\nTo: receiver@example.com\r\nSubject: bytes\r\n\r\nraw\xff\0bytes";
        let media = read_media(AsyncCursor::new(raw), Some(raw.len() as u64)).await.unwrap();
        let mut delegate = Delegate::default();
        let response = hub
            .users()
            .messages_send(
                api::Message {
                    thread_id: Some("thread".into()),
                    ..Default::default()
                },
                "me",
            )
            .delegate(&mut delegate)
            .upload(media, "message/rfc822".parse().unwrap())
            .await;
        let bytes = delegate.response(response).await.unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(delegate.method(), "gmail.users.messages.send");
        let wire = server.await.unwrap();
        let header = String::from_utf8_lossy(&wire[..wire.windows(4).position(|part| part == b"\r\n\r\n").unwrap()]);
        assert!(header.starts_with("POST /upload/gmail/v1/users/me/messages/send?"));
        assert!(header.contains("uploadType=multipart"));
        assert!(header.contains("Bearer test-only-token"));
        assert!(wire.windows(raw.len()).any(|part| part == raw));
        assert!(
            wire.windows(br#""threadId":"thread""#.len())
                .any(|part| part == br#""threadId":"thread""#)
        );
    }

    #[tokio::test]
    async fn collision_json_exchange_preserves_binary_mime_and_draft_metadata() {
        let (hub, address, server) = mock_server("200 OK", r#"{"id":"draft"}"#).await;
        let raw = b"Subject: boundary\r\n\r\n--MDuXWGyeE33QFXGchb2VFWc4Z7945d\r\n\xff\0";
        let draft = api::Draft {
            id: Some("draft".into()),
            message: Some(api::Message {
                thread_id: Some("thread".into()),
                ..Default::default()
            }),
        };
        let body = upload_body(&draft, Some(raw), true).unwrap();
        assert_eq!(
            body.capacity(),
            body.len(),
            "JSON upload allocates its final buffer once"
        );
        let request = json_request(
            &hub,
            Method::PUT,
            &format!("{address}gmail/v1/users/me/drafts/draft"),
            body,
        )
        .await
        .unwrap();
        let response = hub.client.request(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let result = response_bytes(common::Response::from_parts(parts, body.boxed()))
            .await
            .unwrap();
        assert_eq!(result, r#"{"id":"draft"}"#);
        let wire = server.await.unwrap();
        let end = wire.windows(4).position(|part| part == b"\r\n\r\n").unwrap() + 4;
        assert!(wire.starts_with(b"PUT /gmail/v1/users/me/drafts/draft HTTP/1.1"));
        let sent: Value = serde_json::from_slice(&wire[end..]).unwrap();
        assert_eq!(sent["id"], "draft");
        assert_eq!(sent["message"]["threadId"], "thread");
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(sent["message"]["raw"].as_str().unwrap())
                .unwrap(),
            raw
        );
        assert!(sent.get("raw").is_none());
        assert!(sent["message"].get("payload").is_none());
    }

    #[tokio::test]
    async fn provider_quota_response_is_retryable_and_redacted() {
        let (hub, _, server) = mock_server(
            "403 Forbidden",
            r#"{"error":{"errors":[{"reason":"userRateLimitExceeded","message":"private-mail"}]}}"#,
        )
        .await;
        let mut delegate = Delegate::default();
        let result = hub.users().get_profile("me").delegate(&mut delegate).doit().await;
        let error = delegate.response(result).await.unwrap_err();
        assert!(matches!(error, Error::RateLimited { .. }));
        assert!(!format!("{error:?}").contains("private-mail"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn source_size_validation_detects_short_long_empty_and_exact_streams() {
        for (bytes, declared) in [
            (b"a".as_slice(), Some(2)),
            (b"abc", Some(2)),
            (b"", Some(0)),
            (b"", None),
        ] {
            assert!(read_media(AsyncCursor::new(bytes), declared).await.is_err());
        }
        let raw = b"Subject: exact\r\n\r\nbytes";
        let exact = read_media(AsyncCursor::new(raw), Some(raw.len() as u64)).await.unwrap();
        assert_eq!(exact.get_ref(), raw);
        assert_eq!(
            exact.get_ref().capacity(),
            raw.len() + 32,
            "Source buffer retains its initial reservation through EOF"
        );
        assert_eq!(read_media(AsyncCursor::new(raw), None).await.unwrap().into_inner(), raw);
    }

    #[tokio::test]
    async fn response_retains_single_frame_allocation_and_joins_multiple_frames() {
        let bytes = Bytes::from_static(br#"{"id":"single"}"#);
        let pointer = bytes.as_ptr();
        let response = common::Response::new(common::to_body(Some(bytes)));
        assert_eq!(response_bytes(response).await.unwrap().as_ptr(), pointer);
        let frames = vec![
            Ok::<_, HyperError>(Frame::data(Bytes::from_static(b"{\"id\":"))),
            Ok(Frame::data(Bytes::from_static(b"\"multiple\"}"))),
        ];
        let response = common::Response::new(StreamBody::new(stream::iter(frames)).boxed());
        assert_eq!(
            response_bytes(response).await.unwrap(),
            br#"{"id":"multiple"}"#.as_slice()
        );
    }
}
