# Messages contributor notes

Use enzUI widgets and theme tokens for shared controls. Domain rules belong in
crates/messages-core, protocol adapters in messages-protocols, native SDK hosts
in messages-bridge, and device processes in messages-media.

Keep account, room and request generations attached to asynchronous results.
Preserve credential isolation, durable sends, uncertain-send recovery, attachment
custody and account erasure. Never log or commit tokens, keys or account state.

Run formatting, focused behavior tests and strict Clippy. Keep tests for observable
workflows and failure boundaries, consolidate overlaps, and verify adapters with
account-free loopback servers. Fixtures do not establish live-provider support.
Use isolated state for visual checks; never send from a real account for testing.

Translations live in assets/locales/. Curated screenshots belong in
docs/screenshots/; temporary review data stays under ignored AGENTS/. Personal
build limits belong outside tracked files. Stage explicit paths and preserve
existing .local compatibility links and private profiles.
