use enzui_iced_core::placement::{Alignment, Side};
use enzui_iced_widgets::popover::enz_popover;

use super::*;
use crate::app::Popup;

pub(super) fn anchored<'a>(
    app: &'a App,
    trigger: impl Into<Element<'a, Message>>,
    target: Popup,
    alignment: Alignment,
) -> Element<'a, Message> {
    let popup = (app.popup.as_ref() == Some(&target)).then(|| palette(app, target.clone()));
    enz_popover(trigger, popup, || Message::ClosePopup)
        .side(Side::Top)
        .alignment(alignment)
        .width(352.0)
        .padding(8.0)
        .label(if matches!(target, Popup::Emoji) {
            "Emoji"
        } else {
            "Reactions"
        })
        .into_element()
}

fn palette<'a>(app: &'a App, target: Popup) -> Element<'a, Message> {
    let Some(room) = app.selected.as_ref() else {
        return Space::new().into();
    };
    let session = app.session(room.account);
    let room = room.clone();
    enzui_iced_widgets::emoji_picker::enz_emoji_picker(
        &app.emoji_query,
        app.emoji_category,
        &app.emoji_recents,
        move |event| {
            Message::Emoji(crate::app::emoji::Event {
                generation: app.emoji_generation,
                session,
                room: room.clone(),
                target: target.clone(),
                event,
            })
        },
    )
    .id_prefix(crate::app::emoji::picker_prefix(app.emoji_generation))
    .grid_height((app.window_size.height - 270.0).clamp(76.0, 228.0))
    .into_element()
}

pub(super) fn anchored_menu<'a>(
    trigger: Element<'a, Message>,
    menu: Option<Element<'a, Message>>,
    side: Side,
) -> Element<'a, Message> {
    enzui_iced_widgets::anchored_overlay::dismissible_anchored_menu(
        trigger,
        menu,
        enzui_iced_core::placement::Placement::new(side)
            .alignment(Alignment::End)
            .gap(6.0),
        || Message::ClosePopup,
    )
}

pub(super) fn mute_button<'a>(
    app: &'a App,
    room: &messages_core::Conversation,
    in_details: bool,
) -> Element<'a, Message> {
    use enzui_iced_widgets::menu::{EnzMenuItem, enz_menu};
    let visible = if in_details {
        app.dialog == Some(Dialog::Details)
    } else {
        app.dialog.is_none()
    };
    let open = visible && matches!(app.popup, Some(Popup::Mute(_)));
    let token = WidgetId::new("messages-mute-return");
    let trigger = enz_toolbar_icon(if room.muted { "bell-off" } else { "bell" }, Message::Mute)
        .id(WidgetId::new(if in_details {
            "messages-details-mute"
        } else {
            "messages-mute"
        }))
        .aria_label(if room.muted {
            "Unmute notifications"
        } else {
            "Mute notifications"
        })
        .active(open || room.muted)
        .tooltip("Notifications")
        .relinquish_focus_while(open)
        .reclaim_focus_on_return((!open).then_some(token.clone()))
        .into_element();
    let menu = if let Some(Popup::Mute(key)) = app.popup.as_ref().filter(|_| visible) {
        let mut menu = enz_menu()
            .label("Mute notifications")
            .id_prefix("messages-mute-menu")
            .roving_self(true)
            .focus_return_to(token);
        for &(label, duration) in crate::app::customization::MUTE_DURATIONS {
            menu = menu.push_item(EnzMenuItem::command(
                label,
                label,
                Message::MuteFor(key.clone(), duration),
            ));
        }
        Some(menu.into_element())
    } else {
        None
    };
    anchored_menu(trigger, menu, Side::Bottom)
}
