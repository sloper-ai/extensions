# Gmail extension

Use Gmail in the Sloper apps you build for customer follow-ups or mailbox review. Search for messages and label a conversation from a workflow, or send a draft you have prepared. Each successful call writes a `gmail-records` item with the provider response in a managed JSON Source, ready for your app to use.

The extension exposes **79 methods from its pinned Gmail v1 client through 16 Sloper resource actions**, covering mail operations and account settings. Choose an action and its operation, then supply the parameters that Gmail needs. The examples below use the same interface for reading a message and sending mail.

## Calling an action

Call `messages` with `operation: "get"` to read a message. Actions share this parameter shape, with an `operation` selector restricted to the chosen resource:

```json
{
  "operation": "get",
  "path": {"id": "message-id"},
  "query": {"format": "full"},
  "body": {},
  "raw": null
}
```

`path`, `query`, and `body` default to empty objects. `path.userId` defaults to `me`. Keys in `path` match Gmail's reference, including `id`, `messageId`, `sendAsEmail`, `delegateEmail`, `forwardingEmail`, `emailAddress`, and `keyPairId`. Unused or missing path arguments, unsupported queries, and unused bodies or Sources are rejected before requesting credentials.

`body` is a provider-shaped JSON object decoded into the method's generated request type, such as `Label`, `WatchRequest`, `ModifyMessageRequest`, `Draft`, or `SendAs`. The extension does not duplicate those models. Decimal-string fields such as `historyId` and `internalDate` retain Google's string representation. Supported byte fields accept padded and unpadded base64; `raw` MIME for uploads belongs in the direct managed Source property, not inside `body`.

To send a prepared message, pass its managed MIME Source to the `messages` action:

```json
{
  "operation": "send",
  "raw": "managed-source-id",
  "body": {"threadId": "optional-existing-thread-id"}
}
```

The `raw` value must be a Source lent by the host. Its bytes contain the complete RFC 2822/MIME message, including headers, recipients, text, and attachments. To send an already saved draft without replacing its content, use `drafts` with `{"operation":"send","body":{"id":"draft-id"}}` and omit `raw`.

## Coverage

| Action | Operations | Methods |
| --- | --- | ---: |
| `users` | `getProfile`, `stop`, `watch` | 3 |
| `messages` | `batchDelete`, `batchModify`, `delete`, `get`, `import`, `insert`, `list`, `modify`, `send`, `trash`, `untrash` | 11 |
| `attachments` | `get` | 1 |
| `drafts` | `create`, `delete`, `get`, `list`, `send`, `update` | 6 |
| `threads` | `delete`, `get`, `list`, `modify`, `trash`, `untrash` | 6 |
| `history` | `list` | 1 |
| `labels` | `create`, `delete`, `get`, `list`, `patch`, `update` | 6 |
| `settings` | `getImap`, `getLanguage`, `getPop`, `getVacation`, `updateImap`, `updateLanguage`, `updatePop`, `updateVacation` | 8 |
| `auto-forwarding` | `get`, `update` | 2 |
| `filters` | `create`, `delete`, `get`, `list` | 4 |
| `forwarding-addresses` | `create`, `delete`, `get`, `list` | 4 |
| `send-as` | `create`, `delete`, `get`, `list`, `patch`, `update`, `verify` | 7 |
| `delegates` | `create`, `delete`, `get`, `list` | 4 |
| `smime-info` | `delete`, `get`, `insert`, `list`, `setDefault` | 5 |
| `cse-identities` | `create`, `delete`, `get`, `list`, `patch` | 5 |
| `cse-keypairs` | `create`, `disable`, `enable`, `get`, `list`, `obliterate` | 6 |

Queries are forwarded through the generated typed setters. Supported method-specific queries are:

- Message/thread list: `q`, `pageToken`, `maxResults`, `labelIds` (array), `includeSpamTrash`.
- Draft list: `q`, `pageToken`, `maxResults`, `includeSpamTrash`.
- Message/thread get: `format`, `metadataHeaders` (array). Thread format excludes `raw`.
- Draft get: `format`.
- History list: required decimal-string `startHistoryId`, plus `pageToken`, `maxResults`, `labelId`, and `historyTypes` (array).
- Message import: `processForCalendar`, `neverMarkSpam`, `internalDateSource`, `deleted`.
- Message insert: `internalDateSource`, `deleted`.
- CSE identity/keypair list: `pageToken`, `pageSize`.

List calls fetch one page. The complete result envelope retains `nextPageToken` and other pagination metadata; callers pass that token in the next invocation. Watches register an existing authorized Pub/Sub topic. Receiving notifications and renewing watches are separate caller responsibilities. [Gmail reference](https://developers.google.com/workspace/gmail/api/reference/rest), [push notifications](https://developers.google.com/workspace/gmail/api/guides/push)

## Connections and results

Credentials come exclusively from these host-bound connections using the `google.gmail` profile:

| Connection | Scopes | Actions |
| --- | --- | --- |
| `gmail` | `https://mail.google.com/` | Users, messages, attachments, drafts, threads, history, labels |
| `gmail-settings` | `gmail.settings.basic` | Settings, filters |
| `gmail-administration` | `gmail.settings.basic`, `gmail.settings.sharing` | Automatic forwarding, forwarding addresses, send-as, delegates, S/MIME, CSE |

Abbreviated settings scopes use the `https://www.googleapis.com/auth/` prefix. Resource groups that contain delegated mutations use the administration connection for their entire operation selector. Automatic forwarding has a separate action because its update requires the sharing scope. Workspace-specific operations still require Google's account capabilities and delegated authority. This crate does not provision OAuth applications, delegated accounts, Pub/Sub resources, or key services. [Scope reference](https://developers.google.com/workspace/gmail/api/auth/scopes)

Each successful call writes one `gmail-records` item:

| Property | Meaning |
| --- | --- |
| `id` | The Sloper operation identity; the writable resource key, limited to 512 characters |
| `method` | The canonical method name, such as `gmail.users.messages.send` |
| `data` | Managed JSON Source containing the complete provider response envelope |
| `raw` | The original managed MIME Source when supplied; otherwise null |

Successful methods without response bodies produce `{"completed":true}`. Responses are retained from their original bytes, preserving nested MIME data, decimal strings, unknown fields, and base64 spelling. When generated decoding rejects Gmail's valid unpadded base64, the adapter validates a padded copy with the generated model and returns the untouched document. Invalid response JSON still fails.

## Bounds and generated-client adaptations

- Raw MIME Sources are limited to **36,700,160 bytes (35 MiB)**, matching the generated upload ceiling. Known lengths must match the stream exactly; one extra byte detects a dishonest length. Empty MIME is rejected.
- JSON result Sources are independently limited to **33,554,432 bytes (32 MiB)**. Base64 expands raw message data, so a large `format: "raw"` response can exceed this result bound even when an equivalent upload fits. Generated methods buffer and decode their response before the adapter can enforce this limit.
- Request fields absent from the pinned generated models are rejected. This includes classification-label mutations missing from `ModifyMessageRequest` and `BatchModifyMessagesRequest`. Explicit null fields are also rejected because generated request builders remove them. The extension does not claim lossless support for every newer provider request field or null-clearing patch.
- Ordinary MIME uploads use the generated multipart method. The upstream multipart delimiter is fixed; messages containing it use a JSON upload adapter. It preserves the same metadata, query values, and exact MIME bytes. Base64 writes directly into the final JSON buffer. Sending an existing draft without replacement MIME uses that same HTTP client because the generated draft-send builder exposes only upload execution.
- Resumable uploads are not exposed: the pinned common client's chunk helper uses an HTTP method incompatible with Gmail's resumable protocol. No custom resumable state machine or retry loop is maintained here. [Upload protocol](https://developers.google.com/workspace/gmail/api/guides/uploads)

Provider diagnostics are redacted at the Sloper boundary. Authentication failures request reconnection, HTTP 429/5xx and allowlisted Gmail quota errors are retryable, and `Retry-After` is preserved. The extension does not automatically retry mutations.

## Source ownership and verification

The pinned [`google-gmail1` client](https://docs.rs/google-gmail1/7.0.0+20251215/google_gmail1/) supplies Gmail's request models, method builders, and response decoding. This extension adds Sloper parameters, managed Sources, and connection declarations. The [vendored client](../vendor/google-gmail1/PROVENANCE.md) has a manifest-only patch that removes unconditional native certificate loading. The workspace dependency policy documents the `google-apis-common` maintenance advisory [RUSTSEC-2025-0066](https://rustsec.org/advisories/RUSTSEC-2025-0066.html).

`lib.rs` composes the extension. `users.rs` owns the resource actions, Sloper parameter consumption, and output records. `client.rs` configures the generated hub and owns Source bytes, response preservation, and the narrow upload exceptions. `error.rs` owns typed failures and their redacted conversion. There is no second Gmail model or endpoint registry for ordinary operations.

HTTPS uses Hyper and rustls with `ring`, bundled WebPKI roots, and certificate verification. A `std::net::ToSocketAddrs` resolver uses the host's outbound WASI DNS/TCP capabilities; Hyper's default threaded resolver is unsuitable for this runtime. Calls run inside the SDK's existing Tokio runtime. The SDK uses `wasip2` 2.0.0+wasi-0.2.12.

The crate retains its own Mise tasks in this independent public extension workspace. Install the pinned SDK CLI using the repository setup instructions before building. Configure `CC_wasm32_wasip2` in the global Mise environment to an LLVM clang with the WebAssembly backend. The SDK builds the distributable in the release profile to stay below the host's 64 MiB component admission cap.

```sh
mise run '//gmail:build'
mise run '//gmail:test'
cargo clippy -p gmail --all-targets --locked -- -D warnings
cargo clippy -p gmail --lib --target wasm32-wasip2 --locked -- -D warnings
```

Inline unit tests exercise owned request validation, byte handling, original response preservation, quota/error mapping, and real local HTTP exchanges through the generated client. `mise run build` uses the public SDK tool to assemble and stamp the current source into `dist/extension.wasm`. `tests/component.rs` reads that exact artifact and uses the canonical host to check, admit, and invoke it, including scope gates and failures before effects. The component test never rebuilds or restamps the artifact; CI publishes the bytes that passed these tests. Tests use local peers and denied credentials; they never send mail or mutate an account.

## License

Sloper-owned extension code is source-available under the [Sloper Ecosystem License](../LICENSE). Use for Sloper extensions, including business use, is included without a separate SDK license fee. Reuse outside Sloper requires a separate written commercial license. See the [usage guide](../docs/licensing.md); the vendored Google client retains its [MIT license](../vendor/google-gmail1/LICENSE.md).
