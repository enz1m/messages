# Protocol contracts

The desktop uses account-scoped adapters. Signal delegates cryptography and device
linking to signal-cli. Matrix and WhatsApp use local native SDK hosts;
their cryptographic state remains outside the desktop snapshot.

## Signal

The adapter uses JSON-RPC and a resumable SSE connection. It requests buffered
events on first connection and reconnect, retains partial UTF-8/events between
sync windows, and addresses actions with remote message identities. Direct and
existing group conversations support text, bounded file transfer, quotes,
edits/deletes, reactions, delivery/read receipts and typing. Linked-device reads
reduce incoming unread counts; they are not receipts for sent messages.

Pairing completion may commit before its HTTP response fails. The adapter accepts
that outcome only after the helper confirms the linked identity. Linking sessions
and numberless account identifiers stay scoped to the account. Voice/video calls,
group administration, stickers and disappearing/protected content are incomplete.

## Matrix / Element

Managed connections use the Matrix SDK's encrypted store, encryption-aware sends
and encrypted attachment transfer. The REST adapter refuses plaintext sends into
encrypted rooms, including before initial sync.

Homeserver discovery selects advertised password or browser sign-in methods.
Legacy SSO uses an account-bound loopback callback. OAuth uses SDK state/PKCE and
refresh persistence and requires a configured public HTTPS application identity
or server registration. A server advertising both methods can use legacy SSO when
no valid public OAuth identity is configured; an internal placeholder identity is
not registered. OAuth-only registration policies still require deployment setup.

Existing encrypted history requires the device's keys or an existing backup.
Restore encryption keys accepts a recovery key or passphrase without resetting
secret storage. Imported/received keys retry cached ciphertext within its own
account and room; unreadable history is represented by one recovery notice above
readable messages. Redactions and relation retirement remain authoritative during
replay. Interactive device verification and Matrix calls are unavailable.

Message `content_state` values are `available`, `waiting_for_keys` and `relation`.
Missing/null values remain compatible with older snapshots. Managed legacy
snapshots consult their own SDK encryption index before display; English display
text is not used to classify encryption state.

## WhatsApp

The native SDK owns private SQLite/session state, compressed history, media
transfer and live protocol events. Names/profile photos, replies, edits/revokes,
reactions, direct receipts, linked-device reads and typing are supported. Group
receipt aggregation, PN/LID migration and complete historical retrieval are
incomplete. View-once and disappearing content are not copied into local history.

Direct local voice/video transport uses the SDK VoIP engine and bounded media
pipes. Capture starts only when starting/answering a call; end, disconnect and
helper exit retire media. Call UUIDs, room identity and account/session generations
reject stale controls. Remote helpers expose no call capability. Live relay,
incoming-call and provider interoperability remain unqualified.

## Persistence and limits

An outgoing request and its attachment custody are committed before dispatch.
In-flight markers are committed before transport; interrupted non-idempotent
requests recover as uncertain, not as automatic retries. Credentials use the
system keyring or private SDK storage. Async results must still match the account,
room and operation generation that admitted them.

Virtual rows bound widget and preview work, not loaded domain history. Preview
work is limited to two requests and a 64-entry/32 MiB decoded cache; the photo
cache holds at most 64 thumbnails in an 8 MiB file. Device and media subprocesses
stop when their owning session/page closes.

Current performance limits include unconditional sync snapshot writes, opened-file
cache eviction and fully resident loaded history. HTTP responses are bounded to
8 MiB and snapshots to 32 MiB. Safe paging must retain edits, receipts, encryption
keys and send settlement; truncating durable history is not a supported solution.
Variable-height transcript layout can still reconstruct full-history geometry.

Local fixtures and SDK encryption/media tests do not establish live-provider,
installed-system, hardware call or release-performance qualification.
