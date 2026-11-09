# Contributing

The desktop lives in src/app, src/view, src/platform and src/persistence.
messages-core owns domain rules, messages-protocols owns adapters, messages-media
owns device subprocesses and messages-bridge hosts native SDKs in a separate
workspace. Shared widget and platform behavior belongs in enzUI.

Use declared capabilities and check them at dispatch. Keep account, room and
operation generations attached to results. Preserve durable sends, uncertain-send
recovery, keyring isolation, attachment custody and committed account erasure.
Never put credentials, cryptographic keys or real account data into fixtures,
logs, screenshots or Git.

Run the formatting, workspace tests and strict Clippy commands in the README.
Test native helpers separately as described in their guide. Keep workflow and
security-boundary coverage; consolidate overlapping scenarios and avoid checks
that mirror setters or exact presentation strings. Fixture success does not
qualify live provider or hardware support.

Check wide and narrow windows, both color modes, keyboard navigation and native
file drops on the actual binary. Use MESSAGES_STATE_DIR for disposable state;
sample and real accounts must retain separate storage identities. Curated README
screenshots belong in docs/screenshots. Translations live in assets/locales;
German and Arabic coverage is partial.
