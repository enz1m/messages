use messages_core::{
    Attachment, Capability, ChangeKind, Delivery, OutgoingAttachment, ProtocolId, RoomKey,
    SendRequest, fixture,
};
use messages_protocols::{ConnectionConfig, Registry};
use messages_test_server::TestServer;
use serde_json::{Value, json};
use uuid::Uuid;

fn config(server: &TestServer) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(0),
        protocol: ProtocolId::new("signal"),
        label: "Signal".into(),
        endpoint: format!("{}/signal", server.url),
        user_id: "you".into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    }
}

fn event(id: u32, envelope: Value) -> Vec<u8> {
    let value = json!({"params":{"account":"you","envelope":envelope}});
    format!("id: {id}\nevent: receive\ndata: {value}\n\n").into_bytes()
}

#[tokio::test]
async fn signal_file_transfers_preserve_bytes_room_identity_and_preflight_bounds() {
    {
        let server = TestServer::start().await.unwrap();
        server.state.lock().unwrap().signal_chunks = Some(vec![(
            0,
            event(
                1,
                json!({
                    "sourceNumber":"+15550100001", "sourceName":"Maya", "dataMessage":{
                        "timestamp":1791510100000_i64,"message":null,
                        "attachments":[{"id":"fixture-file","filename":"report.txt","contentType":"text/plain","size":13}]
                    }
                }),
            ),
        )]);
        let adapter = Registry::default().connect(config(&server), None).unwrap();
        assert!(adapter.capabilities().supports(Capability::AttachFiles));
        let batch = adapter.sync(None).await.unwrap();
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(
            server
                .state
                .lock()
                .unwrap()
                .requests
                .iter()
                .find(|request| request.path.ends_with("/events"))
                .unwrap()
                .last_event
                .as_deref(),
            Some("0")
        );
        let (room, messages) = &batch.messages[0];
        assert!(messages[0].body.is_empty());
        assert_eq!(messages[0].attachments.len(), 1);
        let file = &messages[0].attachments[0];
        assert_eq!(file.name, "report.txt");
        assert_eq!(
            adapter.attachment(room, file).await.unwrap(),
            b"Hello Signal!"
        );
        {
            let state = server.state.lock().unwrap();
            let request = state.requests.last().unwrap();
            assert_eq!(request.body["method"], "getAttachment");
            assert_eq!(request.body["params"]["recipient"], "+15550100001");
            assert_eq!(request.body["params"]["id"], "fixture-file");
        }
        let other = RoomKey::new(Uuid::new_v4(), &room.room);
        assert!(adapter.attachment(&other, file).await.is_err());
        let mut file = file.clone();
        file.bytes = 12;
        assert!(adapter.attachment(room, &file).await.is_err());
        server.state.lock().unwrap().signal_attachment_data = Some("not base64".into());
        file.bytes = 0;
        assert!(adapter.attachment(room, &file).await.is_err());
    }

    {
        let server = TestServer::start().await.unwrap();
        let directory = std::env::temp_dir().join(format!("messages-signal-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let path = directory.join("report.txt");
        tokio::fs::write(&path, b"Hello Signal!").await.unwrap();
        let request = SendRequest {
            transaction: Uuid::new_v4(),
            room: RoomKey::new(fixture::account_id(0), "+15550100001"),
            body: "A caption".into(),
            reply_to: Some("+15550100001:1791510100000".into()),
            attachments: vec![OutgoingAttachment {
                id: Uuid::new_v4(),
                path,
                name: "report.txt".into(),
                mime: "text/plain".into(),
                bytes: 13,
            }],
        };
        let adapter = Registry::default().connect(config(&server), None).unwrap();
        let sent = adapter.send(&request).await.unwrap();
        assert!(sent.starts_with("you:"));
        {
            let state = server.state.lock().unwrap();
            let params = &state.requests.last().unwrap().body["params"];
            assert_eq!(
                params["attachments"][0],
                "data:text/plain;filename=report.txt;base64,SGVsbG8gU2lnbmFsIQ=="
            );
            assert_eq!(params["message"], "A caption");
            assert_eq!(params["quoteTimestamp"], 1791510100000_i64);
            assert_eq!(params["quoteAuthor"], "+15550100001");
        }
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    {
        let server = TestServer::start().await.unwrap();
        server.state.lock().unwrap().signal_chunks = Some(vec![(
            0,
            event(
                1,
                json!({"sourceNumber":"+15550100001","dataMessage":{
                    "timestamp":1791510100000_i64,"groupInfo":{"groupId":"group-fixture","groupName":"Weekend plans"},"attachments":[{"id":"fixture-file","filename":"clip.mp4","contentType":"video/mp4","size":13}]
                }}),
            ),
        )]);
        let adapter = Registry::default().connect(config(&server), None).unwrap();
        let batch = adapter.sync(None).await.unwrap();
        assert_eq!(batch.messages[0].0.room, "group:group-fixture");
        assert_eq!(
            batch
                .conversations
                .iter()
                .find(|room| room.key.room == "group:group-fixture")
                .unwrap()
                .title,
            "Weekend plans"
        );
        adapter
            .attachment(&batch.messages[0].0, &batch.messages[0].1[0].attachments[0])
            .await
            .unwrap();
        let state = server.state.lock().unwrap();
        let params = &state.requests.last().unwrap().body["params"];
        assert_eq!(params["groupId"], "group-fixture");
        assert!(params.get("recipient").is_none());
    }

    {
        let server = TestServer::start().await.unwrap();
        let adapter = Registry::default().connect(config(&server), None).unwrap();
        let file = Attachment {
            id: "file".into(),
            name: "huge.bin".into(),
            mime: "application/octet-stream".into(),
            bytes: messages_core::MAX_ATTACHMENT_BYTES + 1,
            remote_url: None,
        };
        assert!(
            adapter
                .attachment(&RoomKey::new(fixture::account_id(0), "+15550100001"), &file)
                .await
                .is_err()
        );
        assert!(server.state.lock().unwrap().requests.is_empty());
    }
}

#[tokio::test]
async fn signal_updates_reads_and_replies_keep_account_and_remote_identity() {
    {
        let server = TestServer::start().await.unwrap();
        let target = 1791510100000_i64;
        let source = "+15550100001";
        let envelopes = [
            json!({"sourceNumber":source,"editMessage":{"targetSentTimestamp":target,"dataMessage":{"timestamp":target+100,"message":"Edited text"}}}),
            json!({"sourceNumber":source,"dataMessage":{"timestamp":target+200,"reaction":{"emoji":"👍","targetAuthor":"you","targetSentTimestamp":target,"isRemove":false}}}),
            json!({"sourceNumber":source,"dataMessage":{"timestamp":target+300,"remoteDelete":{"targetTimestamp":target}}}),
            json!({"sourceNumber":source,"receiptMessage":{"isRead":true,"timestamps":[target]}}),
            json!({"sourceNumber":source,"sourceName":"Maya","typingMessage":{"action":"STARTED","timestamp":target+400}}),
            json!({"sourceNumber":source,"syncMessage":{"sentMessage":{"destinationNumber":source,"editMessage":{"targetSentTimestamp":target,"dataMessage":{"timestamp":target+500,"message":"Edited on phone"}}}}}),
        ];
        let mut chunks: Vec<_> = envelopes
            .into_iter()
            .enumerate()
            .map(|(index, envelope)| (0, event(index as u32 + 1, envelope)))
            .collect();
        chunks.push((0, format!("id: 9\ndata: {}\n\n", json!({"params":{"account":"another-account","envelope":{"sourceNumber":source,"dataMessage":{"timestamp":target,"message":"Wrong account"}}}})).into_bytes()));
        server.state.lock().unwrap().signal_chunks = Some(chunks);
        let adapter = Registry::default().connect(config(&server), None).unwrap();
        let batch = adapter.sync(None).await.unwrap();
        assert!(batch.messages.is_empty());
        assert_eq!(batch.changes.len(), 5);
        assert!(
            matches!(&batch.changes[0].kind, ChangeKind::Edit { body, .. } if body == "Edited text")
        );
        assert_eq!(batch.changes[0].message, format!("{source}:{target}"));
        assert!(
            matches!(&batch.changes[1].kind, ChangeKind::Reaction { emoji, own: false, .. } if emoji == "👍")
        );
        assert!(matches!(batch.changes[2].kind, ChangeKind::Delete));
        assert!(matches!(
            batch.changes[3].kind,
            ChangeKind::Receipt(Delivery::Read)
        ));
        assert_eq!(batch.changes[4].message, format!("you:{target}"));
        assert_eq!(batch.typing.len(), 1);
        assert!(batch.typing[0].active);
        assert_eq!(batch.typing[0].name, "Maya");
    }

    {
        let server = TestServer::start().await.unwrap();
        let batch_bytes = [
        event(1, json!({"sourceNumber":"you", "syncMessage":{"readMessages":[
            {"senderNumber":"+15550100001", "timestamp":1791510100000_i64},
            {"senderUuid":"fixture-uuid", "timestamp":1791510101000_i64}
        ]}})),
        event(2, json!({"sourceNumber":"you", "syncMessage":{"sentMessage":{
            "destinationNumber":"+15550100001", "timestamp":1791510110000_i64,"message":"Phone reply"
        }}})),
        event(3, json!({"sourceNumber":"you", "syncMessage":{"sentMessage":{
            "timestamp":1791510120000_i64,"message":"Group reply", "groupInfo":{"groupId":"fixture-group"}
        }}})),
    ].concat();
        let mut wrong_account = json!({"params":{"account":"somebody-else","envelope":{"syncMessage":{"readMessages":[{"sender":"intruder","timestamp":1791510190000_i64}]}}}});
        wrong_account["method"] = json!("receive");
        let wrong = format!("id: 4\nevent: receive\ndata: {wrong_account}\n\n").into_bytes();
        server.state.lock().unwrap().signal_chunks = Some(vec![(0, [batch_bytes, wrong].concat())]);
        let adapter = Registry::default().connect(config(&server), None).unwrap();
        let batch = adapter.sync(None).await.unwrap();
        assert_eq!(batch.device_reads.len(), 2);
        assert_eq!(batch.device_reads[0].sender, "+15550100001");
        assert_eq!(batch.device_reads[1].sender, "fixture-uuid");
        assert!(
            batch
                .device_reads
                .iter()
                .all(|read| read.account == fixture::account_id(0))
        );
        assert_eq!(
            batch.read_through,
            vec![
                (
                    RoomKey::new(fixture::account_id(0), "+15550100001"),
                    1791510110000
                ),
                (
                    RoomKey::new(fixture::account_id(0), "group:fixture-group"),
                    1791510120000
                )
            ]
        );
    }
}

#[tokio::test]
async fn signal_features_protected_content_is_not_retained_in_local_history() {
    let server = TestServer::start().await.unwrap();
    let target = 1791510100000_i64;
    server.state.lock().unwrap().signal_chunks = Some(vec![
        (
            0,
            event(
                1,
                json!({"sourceNumber":"+15550100001","dataMessage":{"timestamp":target,"message":"Private text","expiresInSeconds":30,"attachments":[{"id":"private-file","filename":"private.png","size":100}]}}),
            ),
        ),
        (
            0,
            event(
                2,
                json!({"sourceNumber":"+15550100001","dataMessage":{"timestamp":target+1,"message":null,"viewOnce":true,"attachments":[{"id":null,"filename":"private.png"}]}}),
            ),
        ),
        (
            0,
            event(
                3,
                json!({"sourceNumber":"+15550100001","editMessage":{"targetSentTimestamp":target,"dataMessage":{"timestamp":target+2,"message":"Private edited text","expiresInSeconds":30}}}),
            ),
        ),
    ]);
    let adapter = Registry::default().connect(config(&server), None).unwrap();
    let batch = adapter.sync(None).await.unwrap();
    assert_eq!(batch.messages.len(), 2);
    for (_, messages) in &batch.messages {
        assert_eq!(messages[0].body, "Protected message — open it in Signal.");
        assert!(messages[0].attachments.is_empty());
        assert!(messages[0].reply_to.is_none());
    }
    assert!(batch.changes.is_empty());
}
