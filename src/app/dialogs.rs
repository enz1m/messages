use enzui_iced_core::modal::DismissCause;
use iced::Task;

use super::{App, Dialog, Message};

impl App {
    pub(super) fn open_dialog(&mut self, dialog: Dialog) -> Task<Message> {
        self.shortcut_armed = None;
        self.chooser_start.cancel();
        self.chooser
            .cancel(enzui_iced_widgets::file_chooser::CancelReason::Application);
        if dialog == Dialog::Settings {
            self.settings_drilled_in = false;
        }
        self.cancel_service();
        self.new_request = None;
        self.new_busy = false;
        if matches!(dialog, Dialog::NewThread(_)) {
            self.thread_name.clear();
        }
        self.popup = None;
        self.context_menu.close();
        if !matches!(dialog, Dialog::AccountLogin(_)) {
            self.login = None;
        }
        if dialog == Dialog::NewConversation && self.new_account.is_none() {
            self.new_account = self
                .selected
                .as_ref()
                .map(|key| key.account)
                .or_else(|| self.store.accounts.first().map(|account| account.id));
        }
        let invoker = match &dialog {
            Dialog::Settings
            | Dialog::ConnectAccount
            | Dialog::AccountLogin(_)
            | Dialog::RemoveAccount(_)
            | Dialog::AccountSettings(_) => "messages-settings".to_owned(),
            Dialog::NewConversation => "messages-new-conversation".into(),
            Dialog::DeleteConversation(_) | Dialog::NewThread(_) => "messages-conversations".into(),
            Dialog::Details => "messages-details".into(),
            Dialog::Call => "messages-actions-messages-current-call".into(),
            Dialog::DeleteMessage(_, id) => {
                format!("messages-action-{id}")
            }
        };
        let invoker = if matches!(
            invoker.as_str(),
            "messages-settings"
                | "messages-new-conversation"
                | "messages-shortcuts"
                | "messages-details"
        ) {
            let sidebar = self.window_size.width >= 760.0 && invoker != "messages-details";
            let prefix = if sidebar {
                "messages-sidebar-actions"
            } else {
                "messages-actions"
            };
            if invoker == "messages-shortcuts"
                || if sidebar {
                    self.sidebar_overflow_open
                } else {
                    self.overflow_open
                }
            {
                iced::advanced::widget::Id::from(format!("{prefix}-overflow"))
            } else {
                enzui_iced_core::identity::OwnerId::of_kind(
                    enzui_iced_widgets::action_bar::COMMAND_ID_KIND,
                    prefix,
                )
                .id(&invoker)
            }
        } else {
            iced::advanced::widget::Id::from(invoker)
        };
        let focus = if let Some(previous) = self.dialog.take() {
            self.dialogs.hand_off(&previous, dialog.clone())
        } else {
            self.dialogs.open(dialog.clone(), Some(invoker))
        };
        if dialog != Dialog::Settings {
            self.devices.stop();
        }
        self.dialog = Some(dialog);
        self.overflow_open = false;
        self.sidebar_overflow_open = false;
        focus
    }

    pub(super) fn focus_composer_after_dialog(&mut self) -> Task<Message> {
        self.close_dialog().chain(iced::advanced::widget::operate(
            enzui_iced_core::focus::focus_and_reveal_id(iced::advanced::widget::Id::new(
                "messages-composer",
            )),
        ))
    }

    pub(super) fn close_dialog(&mut self) -> Task<Message> {
        self.devices.stop();
        self.shortcut_armed = None;
        self.cancel_service();
        self.new_request = None;
        self.new_busy = false;
        self.login = None;
        let Some(dialog) = self.dialog.take() else {
            return Task::none();
        };
        self.dialogs.dismiss(&dialog, DismissCause::CloseButton).1
    }
}
