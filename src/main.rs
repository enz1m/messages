mod app;
mod persistence;
mod platform;
mod view;

use enzui_iced_widgets::app_shell::{ShellLayout, enz_window_settings};

fn main() -> iced::Result {
    // Attachment rows draw icon-theme artwork; find the theme off the UI thread.
    enzui_iced_widgets::file_icon::prewarm_icon_theme();
    let mut settings = enz_window_settings(ShellLayout::FloatingTitlebarSidebar);
    settings.exit_on_close_request = false;
    use enzui_iced_widgets::single_instance::{InstanceStart, SingleInstance};
    let path = persistence::state_path();
    let app_id = if std::env::var_os("MESSAGES_STATE_DIR").is_some() {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hash);
        format!("io.enz.Messages.Sandbox.s{:x}", hash.finish())
    } else {
        "io.enz.Messages".to_owned()
    };
    settings = enzui_iced_widgets::app_shell::DesktopApplicationIdentity::new(app_id.clone())
        .expect("valid Messages application identity")
        .apply_to_window_settings(settings);
    let instance = match SingleInstance::start(&app_id) {
        InstanceStart::Forwarded => return Ok(()),
        InstanceStart::Owner(instance, _) => instance,
    };
    let _state_lock = match enzui_iced_core::atomic_state::StateLock::try_acquire(&path) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Ok(()),
        Err(error) => {
            eprintln!("Messages could not lock its saved state: {error}");
            return Ok(());
        }
    };
    let boot = std::sync::Mutex::new(Some(instance));
    enzui_iced_widgets::app_setup::run_with_services(
        iced::application(
            move || {
                let (mut app, task) = app::App::new();
                if let Some(instance) = boot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    app.stop_control = match instance.serve_lifecycle(
                        enzui_iced_core::activation::lifecycle::LifecycleCallers::installed(),
                    ) {
                        Ok(control) => control,
                        Err(error) => {
                            eprintln!("Messages could not serve lifecycle reporting: {error}");
                            None
                        }
                    };
                    app.instance = instance;
                    app.publish_lifecycle();
                }
                (app, task)
            },
            app::App::update,
            view::root,
        )
        .title("Messages")
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(settings),
        enzui_iced_widgets::app_setup::AppServices::default()
            .localizer(platform::localization::localizer()),
    )
}
