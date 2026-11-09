use messages_core::OutgoingAttachment;
use uuid::Uuid;

#[tokio::test]
async fn media_transport_wire_rejects_changed_files_and_size_mismatch() {
    use messages_protocols::media::{WireAttachment, decode, read, validate_total};
    let path = std::env::temp_dir().join(format!("messages-media-{}.txt", Uuid::new_v4()));
    tokio::fs::write(&path, b"new bytes").await.unwrap();
    let file = OutgoingAttachment {
        id: Uuid::new_v4(),
        name: "File.txt".into(),
        mime: "text/plain".into(),
        bytes: 3,
        path: path.clone(),
    };
    assert!(read(&file).await.is_err());
    assert!(validate_total(11, [1; 11]).is_err());
    assert!(validate_total(2, [u64::MAX, 1]).is_err());
    assert!(
        decode(&WireAttachment {
            name: "File".into(),
            mime: "text/plain".into(),
            bytes: 2,
            data: "YQ==".into()
        })
        .is_err()
    );
    assert_eq!(
        decode(&WireAttachment {
            name: "File".into(),
            mime: "text/plain".into(),
            bytes: 1,
            data: "YQ==".into()
        })
        .unwrap(),
        b"a"
    );
    tokio::fs::remove_file(path).await.unwrap();
}
