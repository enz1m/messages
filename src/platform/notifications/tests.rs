use super::*;
use std::time::Duration;
use tokio::sync::mpsc;
use zbus::zvariant::OwnedValue;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

async fn next(client: &Client, queue: &mut std::collections::VecDeque<Event>) -> Option<Event> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(event) = queue.pop_front() {
                return Some(event);
            }
            queue.extend(client.drain());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .ok()
    .flatten()
}

async fn received<T>(receiver: &mut mpsc::Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .expect("notification wire request timed out")
        .expect("fixture connection closed")
}

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command as Process,
};

struct Server {
    next_id: std::sync::atomic::AtomicU32,
    posted: mpsc::Sender<(u32, u32, Vec<String>)>,
    closed: mpsc::Sender<u32>,
    capabilities: Vec<String>,
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        self.capabilities.clone()
    }
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        _app_name: &str,
        replaces_id: u32,
        _app_icon: &str,
        _summary: &str,
        _body: &str,
        actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id == 0 {
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        } else {
            replaces_id
        };
        self.posted.send((id, replaces_id, actions)).await.unwrap();
        id
    }
    async fn close_notification(&self, id: u32) {
        self.closed.send(id).await.unwrap();
    }
}

async fn app_history_and_foreground_are_quiet(
    client: Arc<Client>,
    posted: &mut mpsc::Receiver<(u32, u32, Vec<String>)>,
    closed: &mut mpsc::Receiver<u32>,
) {
    use crate::app::{App, Message};
    use messages_protocols::SyncBatch;

    let (mut app, _) = App::new();
    app.loading = false;
    app.demo = false;
    app.window_focused = false;
    app.notifications = Some(client);
    app.adapters.clear();
    let mut visible = app.store.conversations[0].clone();
    let account = visible.key.account;
    let mut background = visible.clone();
    background.key.room = "notification-background".into();
    let key = background.key.clone();
    let mut message = background.messages[0].clone();
    message.own = false;
    message.deleted = false;
    message.id = "old-unread".into();
    message.timestamp = chrono::Utc::now() - chrono::Duration::hours(1);
    background.messages = vec![message.clone()];
    background.unread = 1;
    visible.messages.clear();
    visible.unread = 0;
    app.selected = Some(visible.key.clone());
    app.store.accounts.retain(|a| a.id == account);
    app.store.conversations = vec![visible, background];
    app.store.chat_states.clear();
    let _ = app.update(Message::Synced(account, Ok(SyncBatch::default())));
    assert_eq!(app.notification_seen.get(&key).unwrap(), "old-unread");
    assert!(app.notification_pending.is_empty());

    let since = app.notification_since[&account];
    for (id, timestamp) in [
        ("late-history", since - chrono::Duration::minutes(1)),
        ("live-background", since + chrono::Duration::milliseconds(1)),
        ("live-foreground", since + chrono::Duration::milliseconds(2)),
        ("next-background", since + chrono::Duration::milliseconds(3)),
    ] {
        message.id = id.into();
        message.timestamp = timestamp;
        let _ = app.update(Message::Synced(
            account,
            Ok(SyncBatch {
                messages: vec![(key.clone(), vec![message.clone()])],
                ..Default::default()
            }),
        ));
        if matches!(id, "late-history" | "live-foreground") {
            assert!(app.notification_pending.is_empty());
            assert!(
                tokio::time::timeout(Duration::from_millis(100), posted.recv())
                    .await
                    .is_err()
            );
            assert_eq!(app.notification_seen.get(&key).unwrap(), id);
        } else {
            assert_eq!(
                app.notification_pending.len(),
                1,
                "{id}: expected a live notification"
            );
            let (server_id, _, _) = received(posted).await;
            tokio::time::timeout(Duration::from_secs(3), async {
                while app.notification_bindings.is_empty() {
                    let _ = app.update(Message::NotificationWake);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let _ = app.update(Message::WindowFocused(true));
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), closed.recv())
                    .await
                    .expect("foreground withdraw reaches notification server"),
                Some(server_id)
            );
            assert!(app.notification_bindings.is_empty());
            assert!(app.notification_pending.is_empty());
            assert!(app.store.room(&key).unwrap().unread > 0);
        }
        if id == "live-foreground" {
            let _ = app.update(Message::WindowFocused(false));
            let _ = app.update(Message::Synced(account, Ok(SyncBatch::default())));
            assert!(app.notification_pending.is_empty());
            assert!(
                tokio::time::timeout(Duration::from_millis(100), posted.recv())
                    .await
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn desktop_notifications_enforce_delivery_policy_and_trusted_server_actions() {
    let mut bus = Process::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(bus.stdout.take().unwrap())
        .read_line(&mut address)
        .await
        .unwrap();
    let address = address.trim();
    let (posted, mut messages) = mpsc::channel(4);
    let (closed, mut closed_ids) = mpsc::channel(4);
    let server = zbus::connection::Builder::address(address)
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            PATH,
            Server {
                next_id: std::sync::atomic::AtomicU32::new(42),
                posted,
                closed,
                capabilities: vec!["body".into(), "actions".into(), "inline-reply".into()],
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = Client::with_address(Some(address.to_owned())).unwrap();
    let mut queue = std::collections::VecDeque::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            client.drain();
            if client.0.lock().unwrap().ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let ticket = Uuid::new_v4();
    assert!(client.post(Notice {
        ticket,
        room: "room".into(),
        title: "Public fixture".into(),
        body: "hello".into(),
        reply: true
    }));
    let (id, replaces, actions) = received(&mut messages).await;
    assert_eq!(replaces, 0);
    assert!(actions.iter().any(|value| value == "inline-reply"));
    assert!(actions.iter().any(|value| value == "Mark as read"));
    assert!(
        matches!(next(&client, &mut queue).await, Some(Event::Posted(value, 1)) if value == ticket)
    );
    let replacement = Uuid::new_v4();
    assert!(client.post(Notice {
        ticket: replacement,
        room: "room".into(),
        title: "Replacement".into(),
        body: "new".into(),
        reply: true
    }));
    let (replacement_id, replaces, replacement_actions) = received(&mut messages).await;
    assert_eq!((replacement_id, replaces), (42, 42));
    assert!(
        matches!(next(&client, &mut queue).await, Some(Event::Posted(value,1)) if value == replacement)
    );
    server
        .emit_signal(
            None::<&str>,
            PATH,
            SERVICE,
            "ActivationToken",
            &(id, "fixture-token"),
        )
        .await
        .unwrap();
    let impostor = zbus::connection::Builder::address(address)
        .unwrap()
        .build()
        .await
        .unwrap();
    impostor
        .emit_signal(
            None::<&str>,
            PATH,
            SERVICE,
            "NotificationReplied",
            &(id, "forged"),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), next(&client, &mut queue))
            .await
            .is_err()
    );
    server
        .emit_signal(
            None::<&str>,
            PATH,
            SERVICE,
            "NotificationReplied",
            &(id, "fixture reply"),
        )
        .await
        .unwrap();
    let Some(Event::Activation(1, token)) = next(&client, &mut queue).await else {
        panic!("missing activation token")
    };
    assert!(!format!("{token:?}").contains("fixture-token"));
    assert_eq!(
        token
            .into_token(std::time::Instant::now())
            .unwrap()
            .select_platform(enzui_iced_core::activation::ActivationPlatform::Wayland)
            .unwrap()
            .into_raw(),
        "fixture-token"
    );
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(3), next(&client, &mut queue)).await.unwrap(), Some(Event::Reply(1, text)) if text == "fixture reply")
    );
    server
        .emit_signal(
            None::<&str>,
            PATH,
            SERVICE,
            "ActionInvoked",
            &(id, replacement_actions[2].as_str()),
        )
        .await
        .unwrap();
    assert!(
        matches!(next(&client, &mut queue).await, Some(Event::Action(1, action)) if action == "mark-read")
    );
    client.close(1);
    assert_eq!(received(&mut closed_ids).await, 42);
    assert!(client.post_test());
    let (_, _, test_actions) = received(&mut messages).await;
    assert!(test_actions.is_empty());
    assert!(matches!(
        next(&client, &mut queue).await,
        Some(Event::TestPosted)
    ));
    app_history_and_foreground_are_quiet(client.clone(), &mut messages, &mut closed_ids).await;
    server.release_name(SERVICE).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), next(&client, &mut queue))
            .await
            .unwrap(),
        Some(Event::Unavailable)
    ));
    bus.kill().await.unwrap();
}
