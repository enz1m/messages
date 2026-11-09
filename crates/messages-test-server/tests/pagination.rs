use messages_core::{ProtocolId, fixture};
use messages_protocols::{ConnectionConfig, Registry};
use messages_test_server::TestServer;

#[tokio::test]
async fn pagination_uses_protocol_cursors_without_skipping_or_repeating_messages() {
    let server = TestServer::start().await.unwrap();
    server.state.lock().unwrap().seed_history(123);
    for (index, protocol) in [(1, "matrix"), (3, "whatsapp")] {
        let account = fixture::account_id(index);
        let room = server
            .state
            .lock()
            .unwrap()
            .store
            .conversations
            .iter()
            .find(|room| room.key.account == account)
            .unwrap()
            .key
            .clone();
        let adapter = Registry::default()
            .connect(
                ConnectionConfig {
                    account,
                    protocol: ProtocolId::new(protocol),
                    label: protocol.into(),
                    endpoint: format!("{}/{protocol}", server.url),
                    user_id: if protocol == "matrix" {
                        "@you:example.test"
                    } else {
                        "you"
                    }
                    .into(),
                    rooms: vec![room.room.clone()],
                    bot: false,
                    native_bridge: false,
                    managed: None,
                },
                Some("test-token".into()),
            )
            .unwrap();
        adapter.sync(None).await.unwrap();
        let mut next = None;
        let mut ids = std::collections::BTreeSet::new();
        let mut pages = 0;
        loop {
            let page = adapter.history_page(&room, next.as_deref()).await.unwrap();
            pages += 1;
            assert!(pages <= 4, "{protocol} cursor failed to advance");
            assert!(page.messages.len() <= 50);
            for message in page.messages {
                assert!(ids.insert(message.id), "{protocol} repeated a message");
            }
            let previous = next;
            next = page.next;
            if next.is_none() {
                break;
            }
            assert_ne!(next, previous);
        }
        assert_eq!(ids.len(), 123, "{protocol} skipped messages");
        assert_eq!(pages, 3);
        if protocol == "matrix" {
            assert!(
                server
                    .state
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .any(|request| request.query.contains("from=offset%3A73"))
            );
        }
    }
}
