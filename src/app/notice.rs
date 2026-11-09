use enzui_iced_widgets::toast::ToastTone;

use super::{App, lifecycle::Session};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Kind {
    Warning,
    Connecting(Session),
    Sync(Session),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notice {
    kind: Kind,
    pub text: String,
}

impl Notice {
    pub fn connecting(session: Session) -> Self {
        Self {
            kind: Kind::Connecting(session),
            text: "Connecting to your account…".into(),
        }
    }

    pub fn sync(session: Session, label: &str) -> Self {
        Self {
            kind: Kind::Sync(session),
            text: format!("{label}: Still trying to reconnect. Messages will retry automatically."),
        }
    }
    pub fn sync_owner(&self, session: Session) -> bool {
        self.kind == Kind::Sync(session)
    }
    // Progress that settles by itself (Info) is a toast that expires; a
    // warning stays until it is closed or its cause clears it.
    pub fn transient(&self) -> bool {
        self.tone() == ToastTone::Info
    }
    pub fn tone(&self) -> ToastTone {
        match self.kind {
            Kind::Warning | Kind::Sync(_) => ToastTone::Warning,
            Kind::Connecting(_) => ToastTone::Info,
        }
    }
}

impl From<String> for Notice {
    fn from(text: String) -> Self {
        Self {
            kind: Kind::Warning,
            text,
        }
    }
}

impl From<&str> for Notice {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl App {
    // Keeps the toast layer in step with `notice` after every update: a
    // transient notice gets a toast, and a toast whose notice was replaced or
    // cleared leaves.
    pub(super) fn present_notice(&mut self) {
        let now = std::time::Instant::now();
        let current = self.notice.as_ref().filter(|notice| notice.transient());
        if self
            .notice_toast
            .as_ref()
            .is_some_and(|(_, shown)| Some(shown) != current)
            && let Some((id, _)) = self.notice_toast.take()
        {
            self.notice_toasts.dismiss(id);
            self.notice_toasts.tick_at(now);
        }
        if self.notice_toast.is_none()
            && let Some(notice) = current.cloned()
        {
            let id = self.notice_toasts.push_at(
                enzui_iced_widgets::toast_host::ToastSpec::new(notice.text.clone())
                    .tone(notice.tone()),
                now,
            );
            self.notice_toast = Some((id, notice));
        }
        self.notice_toasts.sync_expiry(&mut self.notice_expiry, now);
    }

    pub(super) fn clear_connection_notice(&mut self, session: Session) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.kind == Kind::Connecting(session))
        {
            self.notice = None;
        }
    }
}
