use messages_core::{ProtocolId, fixture};
use messages_protocols::{
    ConnectionConfig, ManagedConnection, Registry, local_socket::LocalSocket,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// signal-cli in manual receive mode wraps notifications of a subscription in `result`.
async fn serve(listener: tokio::net::UnixListener, text: &'static str) {
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let reply = if request["method"] == "subscribeReceive" {
                    assert_eq!(request["params"]["account"], "you");
                    format!(
                        "{}\n{}\n",
                        json!({"jsonrpc":"2.0","id":request["id"],"result":0}),
                        json!({"jsonrpc":"2.0","method":"receive","params":{"subscription":0,"result":{
                            "account":"you","envelope":{"source":"+15550100001","sourceName":"Maya",
                            "dataMessage":{"timestamp":1791510100000_i64,"message":text}}}}})
                    )
                } else {
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":request["id"],"result":[]})
                    )
                };
                write.write_all(reply.as_bytes()).await.unwrap();
            }
        });
    }
}

fn config(endpoint: String, managed: Option<ManagedConnection>) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(0),
        protocol: ProtocolId::new("signal"),
        label: "Signal".into(),
        endpoint,
        user_id: "you".into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed,
    }
}

#[tokio::test]
async fn signal_managed_socket_receives_subscribed_messages_and_refuses_unmanaged_sockets() {
    let directory =
        std::env::temp_dir().join(format!("messages-signal-socket-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("helper.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let text = "Over the private socket 👋";
    let server = tokio::spawn(serve(listener, text));
    let endpoint = LocalSocket::endpoint(&path).unwrap();

    assert!(
        Registry::default()
            .connect(config(endpoint.clone(), None), None)
            .is_err()
    );
    let adapter = Registry::default()
        .connect(config(endpoint, Some(ManagedConnection::Signal)), None)
        .unwrap();
    let batch = adapter.sync(None).await.unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].1[0].body, text);

    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
