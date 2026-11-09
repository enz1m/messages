#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let directory = messages_bridge::host::private_directory("whatsapp").await?;
    let token = messages_bridge::host::token(&directory).await?;
    let (backend, bot) = messages_bridge::whatsapp::WhatsApp::prepare(&directory).await?;
    let handle = bot.spawn();
    println!("WhatsApp session directory: {}", directory.display());
    println!("Read access-token from that directory when adding this account to Messages.");
    let result = messages_bridge::host::serve(backend, token, 8821).await;
    let _ = handle.shutdown().await;
    result
}
