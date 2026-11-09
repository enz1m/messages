use enzui_iced_widgets::modal_surface::enz_modal_surface;
use enzui_iced_widgets::scroll_view::enz_scroll_view;
use enzui_iced_widgets::text_field::enz_text_field;
use iced::widget::column;
use messages_protocols::MessageAction;

use super::*;
use crate::app::AccountField;

pub(super) fn dialog<'a>(app: &'a App, dialog: &Dialog) -> Element<'a, Message> {
    let (title, body): (&str, Element<'_, Message>) = match dialog {
        Dialog::Settings => return super::settings::view(app),
        Dialog::Call => ("Call", super::calls::view(app)),
        Dialog::ConnectAccount => ("Connect an account", connect_account(app)),
        Dialog::NewConversation => ("New conversation", super::new_conversation::view(app)),
        Dialog::NewThread(parent) => ("New thread", thread(app, parent)),
        Dialog::Details => ("Conversation details", super::shared::details(app)),
        Dialog::DeleteConversation(key) => return delete_conversation(app, key),
        Dialog::DeleteMessage(key, id) => return delete_message(app, key, id),
        Dialog::AccountLogin(id) => (
            if app.login.as_ref().is_some_and(|login| {
                login.account == *id
                    && matches!(
                        login.state,
                        messages_protocols::login::LoginState::MatrixRecovery
                    )
            }) {
                "Restore encryption keys"
            } else {
                "Connect your account"
            },
            super::account_login::view(app, *id),
        ),
        Dialog::AccountSettings(id) => ("Account settings", super::settings::account(app, *id)),
        Dialog::RemoveAccount(id) => return remove_account(app, *id),
    };
    let radius = enzui_iced_theme::text_defaults::shape_radius(|s| s.dialog);
    let inset = enzui_iced_widgets::modal_surface::close_inset(radius);
    let header = container(enz_text(title).size(TextSize::Title3).width(Length::Fill))
        .height(enzui_iced_widgets::modal_surface::CLOSE_CONTROL_SIZE)
        .align_y(iced::Alignment::Center)
        .padding(
            iced::Padding::ZERO
                .right(enzui_iced_widgets::modal_surface::CLOSE_CONTROL_SIZE + inset),
        );
    let content = column![header, body].spacing(20);
    enz_modal_surface(content.into())
        .on_dismiss(|| Message::CloseDialog)
        .close_button_id(WidgetId::new("messages-dialog-close"))
        .close_label(text("messages-close"))
        .padding(20.0)
        .padding_top(inset)
        .width(Length::Fill.max(460.0))
        .into_element()
}

fn connect_account(app: &App) -> Element<'_, Message> {
    let selected = app
        .registry
        .descriptor(&app.account_form.protocol)
        .map(|item| item.name)
        .unwrap_or("Choose a platform");
    let protocol = column![
        enz_text("Platform").size(TextSize::Body),
        enzui_iced_widgets::menu_button::enz_menu_button(
            selected,
            &app.platform_model,
            &app.platform_menu,
            Message::PlatformMenu
        )
        .aria_label("Platform")
        .menu_id(WidgetId::new("messages-platform-menu"))
        .popup_width(300.0)
        .enabled(app.pending_service.is_none())
        .into_element(),
    ]
    .spacing(6);
    let managed = app.account_form.managed;
    let busy = app.pending_service.is_some();
    let mut form = column![protocol].spacing(12);
    {
        form = form.push(
            enzui_iced_widgets::checkbox::enz_checkbox(
                app.account_form.managed,
                "Manage this connection in Messages",
                |value| Message::AccountField(AccountField::Managed(value)),
            )
            .id(WidgetId::new("messages-connect-managed")),
        );
    }
    if managed {
        match app.account_form.protocol.as_str() {
            "matrix" => {
                form = form.push(matrix_server(app));
                form = form.push(hint("Sign in with the same account you use in Element or another Matrix app. Your password or browser sign-in comes next."));
            }

            "signal" => form = form.push(hint("Link Messages from Signal on your phone using the QR code in the next step. No developer registration is needed.")),
            "whatsapp" => form = form.push(hint("On your phone, open WhatsApp → Linked devices → Link a device, then scan the QR code in the next step.")),
            _ => {}
        }
    } else {
        form = form.push(enz_text_field(
            "Server or local bridge",
            "https://…",
            &app.account_form.endpoint,
            |value| Message::AccountField(AccountField::Endpoint(value)),
        ));
        form = form.push(
            enz_text_field(
                "Access token",
                "Token from your account or local bridge",
                &app.account_form.token,
                |value| Message::AccountField(AccountField::Token(value.into())),
            )
            .secure(true),
        );
        form = form.push(
            enzui_iced_widgets::checkbox::enz_checkbox(
                app.account_form.remember_token,
                "Remember token in the system keyring",
                |remember| Message::AccountField(AccountField::RememberToken(remember)),
            )
            .id(WidgetId::new("messages-connect-remember")),
        );
        if app.account_form.protocol == "matrix" {
            form = form.push(
                enzui_iced_widgets::checkbox::enz_checkbox(
                    app.account_form.native_bridge,
                    "Use the encrypted Matrix SDK bridge",
                    |value| Message::AccountField(AccountField::NativeBridge(value)),
                )
                .id(WidgetId::new("messages-connect-native")),
            );
        }
        if !app.account_form.native_bridge {
            form = form.push(enz_text_field(
                "Your user ID",
                "@you:example.org",
                &app.account_form.user,
                |value| Message::AccountField(AccountField::User(value)),
            ));
        }
        if let Some(descriptor) = app.registry.descriptor(&app.account_form.protocol) {
            form = form.push(hint(descriptor.setup_hint));
        }
    }
    let connect = enz_button(
        if busy {
            "Starting connection…"
        } else if app.account_form.editing.is_some() {
            "Reconnect"
        } else {
            "Connect"
        },
        Message::Connect,
    )
    .id(WidgetId::new("messages-connect-submit"))
    .kind(ButtonKind::Primary)
    .enabled(
        !busy
            && !(managed
                && app.account_form.protocol == "matrix"
                && app.account_form.endpoint.trim().is_empty()),
    );
    if let Some(notice) = &app.credential_notice {
        form = form.push(hint(notice));
    }
    if !app.credential_failed.is_empty() {
        form = form.push(
            enz_button("Retry keyring cleanup", Message::RetryCredentials)
                .id(WidgetId::new("messages-connect-keyring-retry"))
                .kind(ButtonKind::Secondary),
        );
    }
    column![
        enz_scroll_view(form).height(Length::Shrink.max(420.0)),
        row![
            Space::new().width(Length::Fill),
            enz_button("Cancel", Message::CloseDialog)
                .id(WidgetId::new("messages-connect-cancel"))
                .kind(ButtonKind::Quiet),
            connect
        ]
        .spacing(8)
    ]
    .spacing(20)
    .into()
}

fn matrix_server(app: &App) -> Element<'_, Message> {
    use crate::app::matrix_servers::Server;
    let form = &app.account_form;
    let busy = app.pending_service.is_some();
    let select = enzui_iced_widgets::combo_box::enz_combo_box(
        "Server",
        Server::ALL
            .iter()
            .map(|server| server.label().to_owned())
            .collect(),
        Some(form.matrix_server.label().into()),
        |_| Message::AccountField(AccountField::MatrixServer(Server::default())),
    )
    .on_select_index(|index| Message::AccountField(AccountField::MatrixServer(Server::ALL[index])))
    .id(WidgetId::new("messages-matrix-server"))
    .id_prefix("messages-matrix-server")
    .enabled(!busy);
    let mut body = column![select].spacing(8);
    if form.matrix_server == Server::Custom {
        body = body.push(
            enz_text_field(
                "Server address",
                "https://matrix.example.org",
                &form.endpoint,
                |value| Message::AccountField(AccountField::Endpoint(value)),
            )
            .id(WidgetId::new("messages-matrix-custom-server"))
            .enabled(!busy),
        );
        body = body.push(hint("Enter the homeserver URL shown in your Element account settings, or ask your server administrator."));
    } else if form.matrix_server == Server::Mozilla {
        body = body.push(hint("For Matrix IDs ending in :mozilla.org. Continue in your browser to sign in with Mozilla."));
    } else if form.matrix_server == Server::Tchncs {
        body = body.push(hint("For Matrix IDs ending in :tchncs.de. Already use a different server? Choose Custom server…"));
    } else {
        body = body.push(hint("Use this for Matrix IDs ending in :matrix.org, such as @alex:matrix.org. For a work, school or other server, choose Custom server…"));
    }
    body.into()
}

fn remove_account(app: &App, id: messages_core::AccountId) -> Element<'_, Message> {
    let label = app
        .store
        .accounts
        .iter()
        .find(|account| account.id == id)
        .map(|account| account.label.as_str())
        .unwrap_or("this account");
    confirmation(
        app,
        enzui_iced_core::localized!("messages-remove-account-title", "name" => label),
        text("messages-remove-account-body"),
        "messages-remove-account",
        Message::RemoveAccount(id),
    )
}

fn delete_message<'a>(
    app: &'a App,
    key: &messages_core::RoomKey,
    id: &str,
) -> Element<'a, Message> {
    let Some(message) = app.store.room(key).and_then(|room| {
        room.messages
            .iter()
            .find(|item| item.id == id && !item.deleted)
    }) else {
        return hint("This message has already been removed.");
    };
    confirmation(
        app,
        text("messages-delete-message-title"),
        format!(
            "{}\n\n{}",
            text("messages-delete-message-body"),
            message.body.chars().take(220).collect::<String>()
        ),
        "messages-delete-message",
        Message::Action(
            key.clone(),
            MessageAction::Delete {
                message: message.clone(),
            },
        ),
    )
}

fn delete_conversation<'a>(app: &'a App, key: &messages_core::RoomKey) -> Element<'a, Message> {
    let title = app
        .store
        .room(key)
        .map(|room| room.title.as_str())
        .unwrap_or("this conversation");
    confirmation(
        app,
        enzui_iced_core::localized!("messages-delete-history-title", "name" => title),
        text("messages-delete-history-body"),
        "messages-delete",
        Message::DeleteConversation(key.clone()),
    )
}

fn confirmation<'a>(
    app: &'a App,
    title: String,
    body: String,
    label: &str,
    command: Message,
) -> Element<'a, Message> {
    use enzui_iced_widgets::confirm_dialog::{ConfirmDialogEvent, enz_confirm_dialog};
    enz_confirm_dialog(title, body, &app.confirmation, move |event| match event {
        ConfirmDialogEvent::Resolved(enzui_iced_core::modal::DismissCause::DefaultAction) => {
            command.clone()
        }
        ConfirmDialogEvent::Resolved(_) => Message::CloseDialog,
        _ => Message::ActivationFinished,
    })
    .confirm_label(text(label))
    .cancel_label(text("messages-cancel"))
    .destructive(true)
    .id_prefix("messages-confirm")
    .into_surface()
}

fn thread<'a>(app: &'a App, parent: &messages_core::RoomKey) -> Element<'a, Message> {
    let room = app.store.room(parent);
    let key = parent.clone();
    column![
        hint(room.map(|r| r.title.as_str()).unwrap_or("Conversation")),
        enz_text_field(
            "Name",
            "A name for this conversation",
            &app.thread_name,
            Message::ThreadName
        )
        .enabled(!app.new_busy)
        .on_submit(Message::StartThread(key.clone())),
        row![
            Space::new().width(Length::Fill),
            enz_button(
                if app.new_busy {
                    "Creating…"
                } else {
                    "Create"
                },
                Message::StartThread(key)
            )
            .id(WidgetId::new("messages-thread-create"))
            .kind(ButtonKind::Primary)
            .icon("plus")
            .enabled(!app.new_busy && !app.thread_name.trim().is_empty())
        ]
        .spacing(8),
    ]
    .spacing(16)
    .into()
}
