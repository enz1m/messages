//! Explicit sample data shared by the preview and local protocol test server.
//! No account addresses or message text are taken from the host.

use chrono::{TimeZone, Utc};
use uuid::Uuid;

use crate::{
    Account, Capabilities, Capability, ChatMessage, ConnectionState, Conversation, Delivery,
    Encryption, MessageStore, ProtocolId, Reaction, RoomKey, RoomKind,
};

pub const PROTOCOLS: [(&str, &str); 3] = [
    ("signal", "Signal"),
    ("matrix", "Matrix"),
    ("whatsapp", "WhatsApp"),
];

pub fn account_id(index: usize) -> Uuid {
    Uuid::from_u128(0xeeee_0000 + index as u128)
}

pub fn sample_capabilities(_protocol: &str) -> Capabilities {
    let mut capabilities = Capabilities::new([
        Capability::StartConversation,
        Capability::SendText,
        Capability::Reply,
        Capability::React,
        Capability::EditOwn,
        Capability::DeleteOwn,
    ]);
    capabilities.disable(
        Capability::AttachFiles,
        "File transfers are not enabled for this connection.",
    );
    capabilities.disable(
        Capability::VoiceCall,
        "Voice calls are unavailable in this connection.",
    );
    capabilities.disable(
        Capability::VideoCall,
        "Video calls are unavailable in this connection.",
    );

    capabilities
}

pub fn sample_store() -> MessageStore {
    let accounts = PROTOCOLS
        .iter()
        .enumerate()
        .map(|(index, (id, label))| Account {
            id: account_id(if *id == "whatsapp" { 3 } else { index }),
            protocol: ProtocolId::new(*id),
            label: (*label).into(),
            user_id: if *id == "matrix" {
                "@you:example.test".into()
            } else {
                "you".into()
            },
            connection: ConnectionState::Demo,
            capabilities: sample_capabilities(id),
        })
        .collect();
    let mut conversations = vec![
        conversation(
            0,
            "+15550100001",
            "Maya Chen",
            "Signal · Personal",
            RoomKind::Direct,
            true,
            2,
            &[
                (
                    false,
                    "Maya",
                    "Hey! Have you had a chance to look at the plans for Saturday?",
                    9,
                    32,
                ),
                (
                    true,
                    "You",
                    "Just opened them. The coastal walk looks perfect.",
                    9,
                    34,
                ),
                (
                    false,
                    "Maya",
                    "Right? We can grab coffee at the little place by the harbour first.",
                    9,
                    35,
                ),
                (
                    false,
                    "Maya",
                    "Shall we say 10? I’ll bring the camera 📷",
                    9,
                    36,
                ),
                (
                    true,
                    "You",
                    "10 works for me. Looking forward to it!",
                    9,
                    38,
                ),
                (
                    false,
                    "Maya",
                    "Lovely. I’ll send you the pin in the morning.",
                    9,
                    41,
                ),
            ],
        ),
        conversation(
            1,
            "!design:example.test",
            "Design studio",
            "Matrix · 6 members",
            RoomKind::Group,
            true,
            3,
            &[
                (
                    false,
                    "Alex",
                    "The new navigation feels much calmer. Nice work on the spacing.",
                    9,
                    12,
                ),
                (
                    true,
                    "You",
                    "Thanks! I used the same shell as Files and Calendar, so everything feels familiar.",
                    9,
                    14,
                ),
                (
                    false,
                    "Nora",
                    "Can we give the compact layout one more pass before we share it?",
                    9,
                    18,
                ),
                (
                    true,
                    "You",
                    "Absolutely. I’ll check both themes and keyboard navigation too.",
                    9,
                    20,
                ),
            ],
        ),
        conversation(
            3,
            "15550100002@s.whatsapp.net",
            "Family",
            "WhatsApp · 5 members",
            RoomKind::Group,
            false,
            4,
            &[
                (false, "Sam", "Sunday lunch at ours this week?", 8, 52),
                (
                    false,
                    "Mum",
                    "That sounds lovely. I can bring dessert.",
                    8,
                    55,
                ),
                (
                    true,
                    "You",
                    "Count me in! I’ll bring something for the table.",
                    9,
                    2,
                ),
                (false, "Sam", "Perfect, see everyone around one 💛", 9, 8),
            ],
        ),
        conversation(
            0,
            "+15550100003",
            "Alex Rivera",
            "Signal · Personal",
            RoomKind::Direct,
            false,
            0,
            &[
                (false, "Alex", "Thanks for the book recommendation!", 7, 48),
                (
                    true,
                    "You",
                    "You’re welcome. The last chapter is my favourite.",
                    7,
                    52,
                ),
            ],
        ),
        conversation(
            1,
            "!rust:example.test",
            "Rust community",
            "Matrix · Community",
            RoomKind::Group,
            false,
            0,
            &[
                (
                    false,
                    "Jamie",
                    "Today’s tip: make the invalid state impossible to represent.",
                    7,
                    20,
                ),
                (true, "You", "Enums make that a lot nicer.", 7, 24),
            ],
        ),
    ];
    if let Some(message) = conversations[0].messages.get_mut(4) {
        message.reactions.push(Reaction {
            emoji: "💛".into(),
            count: 1,
            own: false,
        });
    }
    if let Some(message) = conversations[1].messages.first_mut() {
        message.reactions.push(Reaction {
            emoji: "👍".into(),
            count: 3,
            own: true,
        });
    }
    MessageStore {
        accounts,
        conversations,
        outbox: Vec::new(),
        revisions: Vec::new(),
        chat_states: Vec::new(),
        device_reads: Vec::new(),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "sample rows keep their named presentation fields together"
)]
fn conversation(
    account: usize,
    id: &str,
    title: &str,
    subtitle: &str,
    kind: RoomKind,
    pinned: bool,
    unread: u32,
    lines: &[(bool, &str, &str, u32, u32)],
) -> Conversation {
    let protocol = match account {
        0 => "signal",
        1 => "matrix",
        3 => "whatsapp",
        _ => unreachable!("unsupported sample account"),
    };
    Conversation {
        key: RoomKey::new(account_id(account), id),
        title: title.into(),
        subtitle: subtitle.into(),
        kind,
        members: vec!["You".into(), title.into()],
        unread,
        pinned,
        muted: false,
        muted_until: None,
        avatar_revision: 0,
        group_name: None,
        parent_room: None,
        archived: false,
        encryption: Encryption::Sample,
        capabilities: sample_capabilities(protocol),
        messages: lines
            .iter()
            .enumerate()
            .map(|(index, (own, sender, body, hour, minute))| ChatMessage {
                content_state: Some(crate::MessageContentState::Available),
                id: format!("sample-{account}-{id}-{index}"),
                transaction: None,
                sender: if *own {
                    "you".into()
                } else {
                    sender.to_lowercase()
                },
                sender_name: (*sender).into(),
                own: *own,
                body: (*body).into(),
                timestamp: Utc
                    .with_ymd_and_hms(2026, 10, 9, *hour, *minute, 0)
                    .unwrap(),
                delivery: if *own {
                    Delivery::Read
                } else {
                    Delivery::Received
                },
                edited: false,
                deleted: false,
                reply_to: None,
                reactions: Vec::new(),
                attachments: Vec::new(),
            })
            .collect(),
        draft: String::new(),
        draft_attachments: Vec::new(),
        reply_to: None,
        has_older: false,
        pagination: None,
    }
}
