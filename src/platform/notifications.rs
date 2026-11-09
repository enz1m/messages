use enzui_iced_core::{
    app_data::AppId,
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

#[derive(Debug)]
pub enum Event {
    Posted(Uuid, u32),
    TestPosted,
    TestFailed,
    Closed(u32),
    Action(u32, String),
    Activation(u32, enzui_iced_core::activation::ActivationContext),
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

struct Pending {
    id: u32,
    notice: Notice,
    actions: bool,
    test: bool,
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
    fn submit(&mut self, pending: Pending) -> bool {
        let mut payload = DesktopNotification::new(local_id(pending.id), &pending.notice.title)
            .body(&pending.notice.body)
            .category("im.received")
            .icon(native::NotificationIcon::Themed(vec![
                "io.enz.Messages".into(),
            ]));
        if pending.actions {
            payload = payload
                .default_action(action("default"))
                .button(NotificationButton::new("Mark as read", action("mark-read")));
            if pending.notice.reply {
                payload = payload.inline_reply(NotificationInlineReply::new(
                    action("reply"),
                    "Reply…",
                    "Send",
                ));
            }
        }
        match self.native.send(payload) {
            Ok(receipt) => {
                self.pending.insert(receipt.operation, pending);
                true
            }
            Err(_) => false,
        }
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
        self.post_with_actions(notice, true)
    }

    pub fn post_test(&self) -> bool {
        self.post_with_actions(
            Notice {
                ticket: Uuid::new_v4(),
                room: "messages:test".into(),
                title: "Messages".into(),
                body: "Desktop notifications are working.".into(),
                reply: false,
            },
            false,
        )
    }

    fn post_with_actions(&self, notice: Notice, actions: bool) -> bool {
        let mut state = self.0.lock().expect("notification state");
        // Service readiness arrives through the native wake subscription.
        if !state.ready || state.pending.len() >= native::MAX_PENDING_NOTIFICATION_OPERATIONS {
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
        state.submit(Pending {
            id,
            notice,
            actions,
            test: !actions,
        })
    }

    pub fn close(&self, id: u32) {
        let mut state = self.0.lock().expect("notification state");
        let _ = state.native.withdraw(&local_id(id));
        state.rooms.retain(|_, value| *value != id);
        state.pending.retain(|_, pending| pending.id != id);
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
                            events.push(if pending.test {
                                Event::TestPosted
                            } else {
                                Event::Posted(pending.notice.ticket, pending.id)
                            });
                        }
                        NotificationOperationOutcome::Unsupported(feature) => {
                            match feature {
                                native::UnsupportedNotificationFeature::InlineReply => {
                                    pending.notice.reply = false
                                }
                                native::UnsupportedNotificationFeature::ActionDelivery => {
                                    pending.actions = false
                                }
                                _ => {
                                    events.push(if pending.test {
                                        Event::TestFailed
                                    } else {
                                        Event::Failed(pending.notice.ticket)
                                    });
                                    continue;
                                }
                            }
                            let ticket = pending.notice.ticket;
                            let test = pending.test;
                            if !state.submit(pending) {
                                events.push(if test {
                                    Event::TestFailed
                                } else {
                                    Event::Failed(ticket)
                                });
                            }
                        }
                        _ => events.push(if pending.test {
                            Event::TestFailed
                        } else {
                            Event::Failed(pending.notice.ticket)
                        }),
                    }
                }
                NotificationEvent::Action(invocation) => {
                    let Ok(id) = invocation.notification_id.as_str().parse() else {
                        continue;
                    };
                    if let Ok(Some(context)) = state.activation.admit_notification_context(
                        invocation.activation_context,
                        invocation.received_at,
                    ) {
                        events.push(Event::Activation(id, context));
                    }
                    if invocation.action_id.as_str() == "reply" {
                        if let Some(text) = invocation.response {
                            events.push(Event::Reply(id, text));
                        }
                    } else {
                        events.push(Event::Action(id, invocation.action_id.as_str().to_owned()));
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
