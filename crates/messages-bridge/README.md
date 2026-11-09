# Native protocol hosts

Matrix and WhatsApp SDK hosts expose an authenticated loopback API to the desktop.
Each account owns its SDK database, cryptographic keys, session and local API token.
Hosts use a separate Cargo workspace to preserve their SDK dependency graph.

Build each helper with Rust 1.99.1 (the helper workspace requires Rust 1.96):

```sh
cargo build --locked \
  --manifest-path crates/messages-bridge/Cargo.toml --target-dir target \
  --features matrix --bin messages-matrix
```

Use feature `whatsapp` and binary `messages-whatsapp` for the other host. The
combined feature set is for metadata collection, not a linked test/helper process.
The desktop normally starts and stops these hosts; manual endpoints default to
ports 8821 for WhatsApp and 8823 for Matrix. `MESSAGES_BRIDGE_STATE` selects private
host storage and `MESSAGES_BRIDGE_TOKEN` supplies a local bearer token. Tokens,
QR/session secrets and provider credentials must not be printed or committed.

`GET /v1/status` negotiates version, identity and capabilities. `GET /v1/chats`
returns cached conversations and live read/typing/call state. Login, history,
sends, media and actions use their corresponding `/v1/` endpoints. Commands enforce
account/room capabilities and input bounds before reaching the SDK. The desktop
also validates account/session generations when accepting replies.

Matrix uses SDK encrypted storage and token refresh, browser state/PKCE, encrypted
room creation and media. Recovery imports existing secret storage and retries
cached ciphertext without a reset. WhatsApp owns its private SQLite session,
protocol events and local direct-call media. [Protocol contracts](../../docs/protocols.md)
describe supported behavior and remaining provider/history limits.

Retained checks use loopback authorities and synthetic cryptographic/media state:

```sh
cargo test --locked \
  --manifest-path crates/messages-bridge/Cargo.toml --target-dir target --features matrix
MESSAGES_MEDIA_TEST=1 cargo test --locked \
  --manifest-path crates/messages-bridge/Cargo.toml --target-dir target --features call-fixtures
```

Tests exercise actual SDK ciphertext/key recovery, private session restart,
SSO/OAuth rejection and cancellation, bounded media, capability refusal and call
cleanup. They do not authorize or qualify real pairing, provider sends or hardware
capture.
