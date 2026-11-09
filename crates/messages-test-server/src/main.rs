use std::sync::{Arc, Mutex};

#[tokio::main(worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    let address = std::env::var("MESSAGES_MOCK_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!(
        "Messages account-free test APIs: http://{}",
        listener.local_addr()?
    );
    println!("Adapters: /matrix, /signal, /whatsapp; token: test-token");
    let state = Arc::new(Mutex::new(messages_test_server::MockState::default()));
    if std::env::var_os("MESSAGES_MOCK_CALLS").is_some() {
        state.lock().expect("fixture state").enable_calls();
    }
    if let Some(path) = std::env::var_os("MESSAGES_MOCK_CALL_IMAGE") {
        use base64::Engine;
        let bytes = tokio::fs::read(path).await?;
        state.lock().expect("fixture state").call_preview =
            Some(base64::engine::general_purpose::STANDARD.encode(bytes));
    }
    if let Some(path) = std::env::var_os("MESSAGES_MOCK_AVATAR") {
        let bytes = tokio::fs::read(path).await?;
        if bytes.len() > 262144 {
            return Err(std::io::Error::other("Fixture photo too large"));
        }
        let mut data = state.lock().expect("fixture state");
        let chats: Vec<_> = data
            .store
            .conversations
            .iter()
            .filter(|r| r.key.account == messages_core::fixture::account_id(3))
            .map(|r| r.key.room.clone())
            .collect();
        for chat in chats {
            data.avatar_data.insert(chat, bytes.clone());
        }
        data.store
            .conversations
            .push(messages_core::Conversation::empty(
                messages_core::RoomKey::new(
                    messages_core::fixture::account_id(3),
                    "15550105555@s.whatsapp.net",
                ),
                "Casey Hart",
                messages_core::RoomKind::Direct,
                messages_core::Encryption::Sample,
                messages_core::fixture::sample_capabilities("whatsapp"),
            ));
    }
    if let Some(count) = std::env::var("MESSAGES_MOCK_HISTORY")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        state
            .lock()
            .expect("mock state available before serving")
            .seed_history(count);
    }
    if std::env::var_os("MESSAGES_MOCK_PAIRING").is_some() {
        let mut data = state.lock().expect("mock state available before serving");
        data.login_steps.insert("whatsapp".into(), "qr".into());
    }
    if std::env::var_os("MESSAGES_MOCK_MEDIA").is_some() {
        let mut data = state.lock().expect("mock state available before serving");
        let room = &mut data.store.conversations[0];
        let mut file = room
            .messages
            .iter()
            .find(|message| !message.own)
            .expect("fixture incoming message")
            .clone();
        file.body = String::new();
        file.timestamp = chrono::Utc::now();
        file.id = "fixture-file-message".into();
        file.attachments = vec![messages_core::Attachment {
            id: "fixture-file".into(),
            name: "Meeting notes.txt".into(),
            mime: "text/plain".into(),
            bytes: 13,
            remote_url: None,
        }];
        room.messages.push(file);
    }
    if let Some(path) = std::env::var_os("MESSAGES_MOCK_IMAGE") {
        use base64::Engine;
        let bytes = tokio::fs::read(path).await?;
        let mut data = state
            .lock()
            .expect("fixture state available before serving");
        data.signal_attachment_data =
            Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
        let room = &mut data.store.conversations[0];
        let mut photo = room
            .messages
            .iter()
            .find(|message| !message.own)
            .expect("fixture incoming message")
            .clone();
        photo.body = "A little escape for the weekend 🌿".into();
        photo.timestamp = chrono::Utc::now();
        photo.id = "fixture-image-message".into();
        photo.attachments = vec![messages_core::Attachment {
            id: "fixture-image".into(),
            name: "Weekend.png".into(),
            mime: "image/png".into(),
            bytes: bytes.len() as u64,
            remote_url: None,
        }];
        room.messages.push(photo);
        if let Some(last) = data.store.conversations[1].messages.last_mut() {
            last.body = "I found a really lovely place for lunch on Sunday, just past the harbour. Shall I book us a table? There is a shaded terrace and a wonderful view of the coast.".into();
        }
    }
    axum::serve(listener, messages_test_server::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
