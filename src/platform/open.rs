use std::{path::PathBuf, time::Duration};

use enzui_iced_core::{
    atomic_state::open_state_file_nofollow,
    portal::{
        CancelReason, PortalOutcome, PortalParent,
        open_uri::{
            OpenFileRequest, OpenUri, OpenUriBackend, OpenUriRequest, PendingOpenUri, RetainedFile,
            desktop_launcher::host_open_uri_backend,
        },
    },
};

const FAILURE: &str = "Could not open the file in another app.";

struct OpenRequest<'a> {
    backend: &'a mut (dyn OpenUriBackend + Send),
    pending: PendingOpenUri,
}

impl Drop for OpenRequest<'_> {
    fn drop(&mut self) {
        // Aborting an account's task must also settle its outstanding portal request.
        self.pending.cancel(CancelReason::Application);
        self.pending.settle(self.backend);
    }
}

pub async fn file(path: PathBuf) -> Result<(), String> {
    let retained = tokio::task::spawn_blocking(move || {
        let file = open_state_file_nofollow(&path).map_err(|_| FAILURE)?;
        RetainedFile::new(file).map_err(|_| FAILURE)
    })
    .await
    .map_err(|_| FAILURE)?
    .map_err(str::to_owned)?;
    let mut backend = host_open_uri_backend();
    let pending = OpenFileRequest::new(retained, PortalParent::None)
        .start(backend.as_mut(), |_| false)
        .map_err(|_| FAILURE)?;
    settle(backend.as_mut(), pending).await
}

pub async fn web(uri: String) -> Result<(), String> {
    messages_protocols::login::validate_browser_url(&uri)
        .map_err(|_| "The sign-in URL is not safe to open.".to_owned())?;
    let mut backend = host_open_uri_backend();
    let uri = OpenUri::parse(uri).map_err(|_| "The sign-in URL is invalid.".to_owned())?;
    let pending = OpenUriRequest::new(uri, PortalParent::None)
        .start(backend.as_mut(), |_| false)
        .map_err(|_| "Could not open your browser.".to_owned())?;
    settle(backend.as_mut(), pending)
        .await
        .map_err(|_| "Could not open your browser. Try Open browser again.".into())
}

async fn settle(
    backend: &mut (dyn OpenUriBackend + Send),
    pending: PendingOpenUri,
) -> Result<(), String> {
    let mut request = OpenRequest { backend, pending };
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let signal = request.backend.open_uri_completion_signal();
            let seen = signal.as_ref().map(|signal| signal.generation());
            if let Some(outcome) = request.pending.poll(request.backend) {
                return match outcome {
                    PortalOutcome::Completed(()) | PortalOutcome::Cancelled(_) => Ok(()),
                    _ => Err(FAILURE.to_owned()),
                };
            }
            if let (Some(signal), Some(seen)) = (signal, seen) {
                signal.wait_after(seen).await;
            } else {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    })
    .await
    .map_err(|_| FAILURE)?
}
