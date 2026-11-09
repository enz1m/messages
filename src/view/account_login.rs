use enzui_iced_widgets::{qr::enz_qr, text_field::enz_text_field};
use iced::widget::column;
use messages_core::AccountId;
use messages_protocols::login::LoginState;

use super::*;

pub(super) fn view(app: &App, account: AccountId) -> Element<'_, Message> {
    let Some(login) = app.login.as_ref().filter(|login| login.account == account) else {
        return hint("Loading sign-in…");
    };
    let platform = app
        .store
        .accounts
        .iter()
        .find(|entry| entry.id == account)
        .map(|entry| entry.label.as_str())
        .unwrap_or("Account");
    let mut submit = None;
    let mut content = column![enz_text(platform).size(TextSize::Headline)].spacing(16);
    match &login.state {
        LoginState::MatrixRecovery => {
            content = content
                .push(hint("Restore encrypted message history using the recovery key or passphrase from Element's Security & Privacy settings. Your account password is different. Existing encryption keys and backups will not be reset."))
                .push(enz_text_field("Recovery key or passphrase", "Enter your recovery key or passphrase", &login.value, |value| Message::LoginValue(value.into())).secure(true).enabled(!login.busy).on_submit(Message::LoginSubmit))
                .push(enz_button(if login.busy { "Restoring keys…" } else { "Restore encryption keys" }, Message::LoginSubmit).id(WidgetId::new("messages-matrix-recover-submit")).kind(ButtonKind::Primary).enabled(!login.busy && !login.value.trim().is_empty()))
                .push(hint("If no recovery key is available, keep your existing Element session signed in. Verification from another device is not yet available in Messages."));
        }
        LoginState::Qr { .. } => {
            content = content.push(hint(
                if app
                    .connections
                    .iter()
                    .any(|config| config.account == account && config.protocol.as_str() == "signal")
                {
                    "On your phone, open Signal → Settings → Linked devices, then scan this code."
                } else {
                    "On your phone, open WhatsApp → Linked devices → Link a device."
                },
            ));
            if login
                .qr_expires
                .is_some_and(|expiry| expiry > std::time::Instant::now())
            {
                if let Some(qr) = &login.qr {
                    content = content.push(
                        container(enz_qr(qr).total_size(220.0))
                            .width(Length::Fill)
                            .align_x(iced::alignment::Horizontal::Center),
                    );
                } else {
                    content = content.push(hint("This pairing code could not be displayed."));
                }
            } else {
                content = content.push(hint(
                    "This code has expired. A fresh code is being requested.",
                ));
            }
        }
        LoginState::Browser { .. } => {
            content = content
                .push(hint("Finish signing in in your browser, then return here. Your password stays with your homeserver."))
                .push(row![
                    enz_button("Open browser", Message::BrowserOpen)
                        .id(WidgetId::new("messages-browser-open"))
                        .kind(ButtonKind::Primary),
                    enz_button("Cancel browser sign-in", Message::BrowserCancel)
                        .id(WidgetId::new("messages-browser-cancel"))
                        .kind(ButtonKind::Quiet),
                ].spacing(8));
        }
        LoginState::Matrix {
            password: false, ..
        } => {}
        LoginState::Unsupported { reason } => content = content.push(hint(reason)),
        LoginState::Connecting => {
            content = content.push(hint("Waiting for the native connection…"))
        }
        LoginState::Ready => content = content.push(hint("Connected. Loading conversations…")),
        LoginState::Phone
        | LoginState::Code
        | LoginState::Password
        | LoginState::Matrix { password: true, .. } => {
            let matrix = app
                .connections
                .iter()
                .any(|config| config.account == account && config.protocol.as_str() == "matrix");
            let (label, placeholder, secure) = match &login.state {
                LoginState::Phone => ("Phone number", "+44…", false),
                LoginState::Code => ("Login code", "Verification code", true),
                _ => (
                    "Password",
                    if matrix {
                        "Matrix account password"
                    } else {
                        "Account password"
                    },
                    true,
                ),
            };
            if matrix {
                content = content.push(
                    enz_text_field(
                        "Matrix user ID",
                        "@you:server",
                        &login.user,
                        Message::LoginUser,
                    )
                    .enabled(!login.busy),
                );
            }
            content = content.push(
                enz_text_field(label, placeholder, &login.value, |value| {
                    Message::LoginValue(value.into())
                })
                .secure(secure)
                .enabled(!login.busy)
                .on_submit(Message::LoginSubmit),
            );
            submit = Some(
                enz_button(
                    if login.busy {
                        "Signing in…"
                    } else {
                        "Continue"
                    },
                    Message::LoginSubmit,
                )
                .id(WidgetId::from(format!("messages-login-submit-{account}")))
                .kind(ButtonKind::Primary)
                .enabled(!login.busy && !login.value.is_empty())
                .into_element(),
            );
        }
    }
    if let LoginState::Matrix {
        browser, problem, ..
    } = &login.state
    {
        if *browser {
            content = content.push(
                enz_button("Sign in with browser", Message::BrowserBegin)
                    .id(WidgetId::new("messages-matrix-browser"))
                    .kind(ButtonKind::Primary)
                    .enabled(!login.busy && !login.loading),
            );
        }
        if let Some(problem) = problem {
            content = content.push(hint(problem));
        }
    }
    if let Some(error) = &login.error {
        content = content.push(hint(error));
    }
    let mut actions = row![
        enz_button(
            if matches!(login.state, LoginState::MatrixRecovery) {
                "Not now"
            } else {
                "Refresh connection"
            },
            if matches!(login.state, LoginState::MatrixRecovery) {
                Message::CloseDialog
            } else {
                Message::LoginLoad(account)
            }
        )
        .id(WidgetId::from(format!("messages-login-refresh-{account}")))
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .enabled(!login.loading && !login.busy),
        Space::new().width(Length::Fill)
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if let Some(submit) = submit {
        actions = actions.push(submit);
    }
    content.push(actions).into()
}
