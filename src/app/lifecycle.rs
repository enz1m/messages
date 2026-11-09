use iced::{Task, widget::text_editor};
use messages_core::{AccountId, OutboxState, SendError};
use uuid::Uuid;

use super::{App, Message};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Session {
    pub account: AccountId,
    generation: Uuid,
}

impl Session {
    pub fn wrap(self, message: Message) -> Message {
        Message::SessionResult(self, Box::new(message))
    }
}

impl App {
    pub(crate) fn session(&self, account: AccountId) -> Session {
        Session {
            account,
            generation: self.sessions.get(&account).copied().unwrap_or_default(),
        }
    }

    pub(super) fn session_is_current(&self, session: Session) -> bool {
        self.store
            .accounts
            .iter()
            .any(|account| account.id == session.account)
            && self.session(session.account) == session
    }

    pub(super) fn renew_session(&mut self, account: AccountId) {
        self.retire_avatars(account);
        if self
            .calls
            .active
            .as_ref()
            .is_some_and(|call| call.room.account == account)
        {
            self.retire_call();
        }
        if self
            .context_menu
            .target()
            .is_some_and(|target| match target {
                super::ContextTarget::Conversation(key) | super::ContextTarget::Message(key, _) => {
                    key.account == account
                }
            })
        {
            self.context_menu.close();
        }
        self.chooser_start.cancel();
        self.drop_admissions
            .retain(|_, admission| admission.session.account != account);
        self.chooser
            .cancel(enzui_iced_widgets::file_chooser::CancelReason::Application);
        let rooms: Vec<_> = self
            .store
            .conversations
            .iter()
            .filter(|room| room.key.account == account)
            .map(|room| room.key.clone())
            .collect();
        for key in rooms {
            self.withdraw_room_notification(&key);
        }
        self.notification_since.remove(&account);
        self.sessions.insert(account, Uuid::new_v4());
        self.typing.retain(|(key, _), _| key.account != account);
        self.typing_sent.retain(|key, _| key.account != account);
        self.read_sent.retain(|(key, _), _| key.account != account);
        self.preview_tasks
            .retain(|key, _| key.room.account != account);
        self.previews.retain(|key, _| key.room.account != account);
        self.visible_previews
            .retain(|key| key.room.account != account);
        self.syncing.remove(&account);
        self.sync_retry.remove(&account);
        self.managed_login_checks.remove(&account);
        self.file_tasks
            .retain(|purpose, _| purpose.session().account != account);
        self.file_busy.retain(|key| key.account != account);
        self.file_operations
            .retain(|purpose| purpose.session().account != account);
        self.history_busy.retain(|key| key.account != account);
        self.history_errors.retain(|key, _| key.account != account);
        self.credential_restoring.remove(&account);
        self.cursors.remove(&account);
        let interrupted: Vec<_> = self
            .store
            .outbox
            .iter()
            .filter(|item| {
                item.request.room.account == account && item.state == OutboxState::Sending
            })
            .map(|item| (item.request.room.clone(), item.request.transaction))
            .collect();
        for (room, transaction) in interrupted {
            self.store
                .fail_send(&room, transaction, SendError::Uncertain);
        }
        self.dispatch_after_save.retain(|transaction| {
            self.store.outbox.iter().any(|item| {
                item.request.transaction == *transaction && item.request.room.account != account
            })
        });
        self.send_admissions.retain(|transaction, _| {
            self.store.outbox.iter().any(|item| {
                item.request.transaction == *transaction && item.request.room.account != account
            })
        });
        if self.new_request.is_some_and(|(_, owner)| owner == account) {
            self.new_request = None;
            self.new_busy = false;
        }
    }

    pub(super) fn remove_account(&mut self, account: AccountId) -> Task<Message> {
        if self.loading || self.persistence_blocked {
            return Task::none();
        }
        self.renew_session(account);
        self.sessions.remove(&account);
        self.notification_seen
            .retain(|key, _| key.account != account);
        self.avatars.forget(account);
        self.adapters.remove(&account);
        if let Some(host) = self.managed_hosts.remove(&account) {
            self.erase_hosts.insert(account, host);
        }
        self.release_credential(account);
        self.store.accounts.retain(|entry| entry.id != account);
        self.store.forget_revisions(account);
        self.store
            .chat_states
            .retain(|state| state.key.account != account);
        self.store
            .device_reads
            .retain(|read| read.account != account);
        self.store
            .conversations
            .retain(|room| room.key.account != account);
        self.store
            .outbox
            .retain(|item| item.request.room.account != account);
        self.connections.retain(|config| config.account != account);
        self.preferences.accounts.remove(&account);
        self.scroll_positions
            .retain(|key, _| key.account != account);
        if self.new_account == Some(account) {
            self.new_account = None;
            self.new_recipient.clear();
        }
        if self.account_form.editing == Some(account) {
            self.account_form = super::AccountForm::default();
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|key| key.account == account)
        {
            self.selected = self
                .store
                .conversations
                .first()
                .map(|room| room.key.clone());
            self.editor = text_editor::Content::with_text(
                self.current_room()
                    .map(|room| room.draft.as_str())
                    .unwrap_or(""),
            );
            self.editing = None;
            self.show_detail = false;
        }
        self.notice = None;
        self.changed();
        self.schedule_account_erasure(account, self.revision);
        Task::batch([self.save(), self.close_dialog()])
    }
}
