use super::*;
use crate::app::ContextTarget;
use messages_core::Capability;

pub(super) fn host<'a>(app: &'a App, shell: Element<'a, Message>) -> Element<'a, Message> {
    app.context_menu
        .host(shell, Message::ContextEvent)
        .label(
            if matches!(
                app.context_menu.target(),
                Some(ContextTarget::Message(_, _))
            ) {
                "Message actions"
            } else {
                "Conversation actions"
            },
        )
        .header(move |_| {
            themed(app, || {
                let can_react = matches!(app.context_menu.target(),
                    Some(ContextTarget::Message(key, _)) if app.store.capabilities(key).supports(Capability::React));
                let mut reactions = row![].spacing(2);
                if can_react {
                    for emoji in ["👍", "❤️", "😂", "😮", "😢", "🙏"] {
                        reactions = reactions.push(
                            enz_button(
                                emoji,
                                Message::ContextReaction(app.context_showing, emoji.into()),
                            )
                            .id(WidgetId::from(format!("messages-context-reaction-{emoji}")))
                            .kind(ButtonKind::Quiet)
                            .size_tokens(enzui_iced_widgets::tokens::ButtonSizeTokens {
                                height: 34.0,
                                padding_x: 5.0,
                                text_size: 20.0,
                                radius: enzui_iced_theme::text_defaults::shape_radius(|s| s.button),
                                ..ButtonSize::Sm.tokens()
                            }),
                        );
                    }
                }
                if can_react {
                    iced::widget::column![reactions, enzui_iced_widgets::divider::enz_divider()]
                        .spacing(6).into()
                } else { Space::new().into() }
            })
        })
        .into_element()
}
