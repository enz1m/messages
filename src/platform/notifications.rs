use enzui_iced_core::{
    activation::ActivationContext,
    app_data::AppId,
    localization::text,
    portal::notification::{
        self as native, DesktopNotification, DesktopNotificationClient, NotificationAction,
        NotificationActionId, NotificationButton, NotificationEvent, NotificationId,
        NotificationInlineReply, NotificationOperationId, NotificationOperationOutcome,
        freedesktop::FreedesktopNotificationBackend,
    },
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const TEST_ID: &str = "notification-test";
const OPEN_SETTINGS: &str = "open-notification-settings";

#[derive(Debug)]
pub enum Event {
    Posted(Uuid, u32),
    TestPosted,
    TestFailed,
    Closed(u32),
    Action(u32, String),
    Activation(u32, ActivationContext),
    OpenNotificationSettings(Option<ActivationContext>),
    Reply(u32, String),
    Failed(Uuid),
    Unavailable,
}

pub struct Notice {
    pub ticket: Uuid,
    pub room: String,
    pub title: String,
    pub body: String,
    pub reply: bool,
}

enum Post {
    Conversation { id: u32, notice: Notice },
    Test,
}

struct Pending {
    post: Post,
    actions: bool,
}

impl Post {
    fn result(self, sent: bool) -> Event {
        match (self, sent) {
            (Self::Conversation { id, notice }, true) => Event::Posted(notice.ticket, id),
            (Self::Conversation { notice, .. }, false) => Event::Failed(notice.ticket),
            (Self::Test, true) => Event::TestPosted,
            (Self::Test, false) => Event::TestFailed,
        }
    }
}

struct State {
    native: DesktopNotificationClient<FreedesktopNotificationBackend>,
    rooms: HashMap<String, u32>,
    pending: HashMap<NotificationOperationId, Pending>,
    next_id: u32,
    ready: bool,
    activation: enzui_iced_core::activation::ActivationBroker,
}

pub struct Client(Mutex<State>);
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DesktopNotifications")
    }
}

fn local_id(id: u32) -> NotificationId {
    NotificationId::new(id.to_string()).expect("numeric notification identity")
}
fn action(id: &str) -> NotificationAction {
    NotificationAction::new(NotificationActionId::new(id).expect("fixed action identity"))
}

impl State {
    fn submit(&mut self, pending: Pending) -> Result<(), Post> {
        let payload = match &pending.post {
            Post::Conversation { id, notice } => {
                let mut payload = DesktopNotification::new(local_id(*id), &notice.title)
                    .body(&notice.body)
                    .category("im.received");
                if pending.actions {
                    payload =
                        payload
                            .default_action(action("default"))
                            .button(NotificationButton::new(
                                text("messages-notification-mark-read"),
                                action("mark-read"),
                            ));
                    if notice.reply {
                        payload = payload.inline_reply(NotificationInlineReply::new(
                            action("reply"),
                            text("messages-notification-reply"),
                            text("messages-notification-send"),
                        ));
                    }
                }
                payload
            }
            Post::Test => {
                let mut payload = DesktopNotification::new(
                    NotificationId::new(TEST_ID).expect("fixed test identity"),
                    text("messages-notification-test-title"),
                )
                .body(text(if pending.actions {
                    "messages-notification-test-body"
                } else {
                    "messages-notification-test-body-passive"
                }));
                if pending.actions {
                    payload = payload.default_action(action(OPEN_SETTINGS));
                }
                payload
            }
        }
        .icon(native::NotificationIcon::Themed(vec![
            "io.enz.Messages".into(),
        ]));
        match self.native.send(payload) {
            Ok(receipt) => {
                self.pending.insert(receipt.operation, pending);
                Ok(())
            }
            Err(_) => Err(pending.post),
        }
    }

    fn can_submit(&self) -> bool {
        self.ready && self.pending.len() < native::MAX_PENDING_NOTIFICATION_OPERATIONS
    }
}

impl Client {
    pub async fn connect() -> Result<Arc<Self>, String> {
        Self::with_address(None)
    }

    fn with_address(address: Option<String>) -> Result<Arc<Self>, String> {
        let app = AppId::new("io.enz.Messages").expect("application identity");
        let backend = FreedesktopNotificationBackend::with_address(app.clone(), address)
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self(Mutex::new(State {
            native: DesktopNotificationClient::new(app, backend),
            rooms: HashMap::new(),
            pending: HashMap::new(),
            next_id: 0,
            ready: false,
            activation: Default::default(),
        }))))
    }

    pub fn subscription(&self) -> iced::Subscription<crate::app::Message> {
        self.0
            .lock()
            .expect("notification state")
            .native
            .wake_subscription()
            .map(|_| crate::app::Message::NotificationWake)
    }

    pub fn ready(&self) -> bool {
        self.0.lock().expect("notification state").ready
    }

    pub fn post(&self, notice: Notice) -> bool {
        let mut state = self.0.lock().expect("notification state");
        if !state.can_submit() {
            return false;
        }
        let id = if let Some(id) = state.rooms.get(&notice.room) {
            *id
        } else {
            if state.rooms.len() >= native::MAX_ACTIVE_NOTIFICATIONS {
                return false;
            }
            let Some(id) = state.next_id.checked_add(1) else {
                return false;
            };
            state.next_id = id;
            state.rooms.insert(notice.room.clone(), id);
            id
        };
        state
            .submit(Pending {
                post: Post::Conversation { id, notice },
                actions: true,
            })
            .is_ok()
    }

    pub fn post_test(&self) -> bool {
        let mut state = self.0.lock().expect("notification state");
        state.can_submit()
            && state
                .submit(Pending {
                    post: Post::Test,
                    actions: true,
                })
                .is_ok()
    }

    pub fn close(&self, id: u32) {
        let mut state = self.0.lock().expect("notification state");
        let _ = state.native.withdraw(&local_id(id));
        state.rooms.retain(|_, value| *value != id);
        state.pending.retain(|_, pending| {
            !matches!(&pending.post, Post::Conversation { id: posted, .. } if *posted == id)
        });
    }

    pub fn drain(&self) -> Vec<Event> {
        let mut state = self.0.lock().expect("notification state");
        let mut events = Vec::new();
        while let Some(event) = state.native.poll_event() {
            match event {
                NotificationEvent::Closed {
                    notification_id, ..
                } => {
                    if let Ok(id) = notification_id.as_str().parse() {
                        state.rooms.retain(|_, value| *value != id);
                        events.push(Event::Closed(id));
                    }
                }
                NotificationEvent::Operation { receipt, outcome } => {
                    let Some(mut pending) = state.pending.remove(&receipt.operation) else {
                        continue;
                    };
                    if state
                        .native
                        .current_generation(&receipt.notification_id)
                        .is_some_and(|g| g > receipt.generation)
                    {
                        continue;
                    }
                    match outcome {
                        NotificationOperationOutcome::Sent
                        | NotificationOperationOutcome::Replaced => {
                            events.push(pending.post.result(true));
                        }
                        NotificationOperationOutcome::Unsupported(feature) => {
                            match (feature, &mut pending.post) {
                                (
                                    native::UnsupportedNotificationFeature::InlineReply,
                                    Post::Conversation { notice, .. },
                                ) if notice.reply => notice.reply = false,
                                (native::UnsupportedNotificationFeature::ActionDelivery, _)
                                    if pending.actions =>
                                {
                                    pending.actions = false;
                                }
                                _ => {
                                    events.push(pending.post.result(false));
                                    continue;
                                }
                            }
                            if let Err(post) = state.submit(pending) {
                                events.push(post.result(false));
                            }
                        }
                        _ => events.push(pending.post.result(false)),
                    }
                }
                NotificationEvent::Action(invocation) => {
                    let context = state
                        .activation
                        .admit_notification_context(
                            invocation.activation_context,
                            invocation.received_at,
                        )
                        .ok()
                        .flatten();
                    let action_id = invocation.action_id.as_str();
                    if invocation.notification_id.as_str() == TEST_ID {
                        if action_id == OPEN_SETTINGS {
                            events.push(Event::OpenNotificationSettings(context));
                        }
                        continue;
                    }
                    let Ok(id) = invocation.notification_id.as_str().parse() else {
                        continue;
                    };
                    if let Some(context) = context {
                        events.push(Event::Activation(id, context));
                    }
                    if action_id == "reply" {
                        if let Some(text) = invocation.response {
                            events.push(Event::Reply(id, text));
                        }
                    } else {
                        events.push(Event::Action(id, action_id.to_owned()));
                    }
                }
                NotificationEvent::ServiceChanged {
                    available: true, ..
                } => state.ready = true,
                NotificationEvent::ServiceChanged {
                    available: false, ..
                }
                | NotificationEvent::ActionDeliveryUnavailable => {
                    state.ready = false;
                    state.rooms.clear();
                    state.pending.clear();
                    events.push(Event::Unavailable);
                }
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests;
