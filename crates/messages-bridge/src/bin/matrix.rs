#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let homeserver =
        std::env::var("MATRIX_HOMESERVER").unwrap_or_else(|_| "https://matrix.org".into());
    let directory = messages_bridge::host::private_directory("matrix").await?;
    let token = messages_bridge::host::token(&directory).await?;
    let client_uri = std::env::var("MESSAGES_MATRIX_CLIENT_URI").ok();
    let backend = messages_bridge::matrix::Matrix::prepare_with_client_uri(
        &directory,
        &homeserver,
        client_uri.as_deref(),
    )
    .await?;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(backend.clone().run(receiver));
    println!("Matrix session directory: {}", directory.display());
    let result = messages_bridge::host::serve(backend, token, 8823).await;
    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(20), worker).await;
    result
}
