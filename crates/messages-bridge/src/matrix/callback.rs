use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use url::Url;

pub(super) async fn receive(
    listener: &TcpListener,
    redirect: &Url,
    expected_state: Option<&str>,
) -> anyhow::Result<(TcpStream, Url)> {
    loop {
        let (mut stream, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            read(&mut stream, redirect),
        )
        .await;
        if let Ok(Ok(uri)) = result {
            let state = uri
                .query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned());
            if expected_state.is_none() || state.as_deref() == expected_state {
                return Ok((stream, uri));
            }
        }
        let _ = respond(&mut stream, false).await;
    }
}

async fn read(stream: &mut TcpStream, redirect: &Url) -> anyhow::Result<Url> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0u8; 512];
        let n = stream.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "Incomplete callback");
        bytes.extend_from_slice(&chunk[..n]);
        anyhow::ensure!(bytes.len() <= 8192, "Callback too large");
        if bytes.windows(4).any(|p| p == b"\r\n\r\n") {
            break;
        }
    }
    validate(std::str::from_utf8(&bytes)?, redirect)
}

pub(super) fn validate(request: &str, redirect: &Url) -> anyhow::Result<Url> {
    let mut lines = request.split("\r\n");
    let mut start = lines.next().unwrap_or_default().split_whitespace();
    anyhow::ensure!(
        start.next() == Some("GET"),
        "Callback method is not allowed"
    );
    let target = start
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing callback target"))?;
    anyhow::ensure!(
        target.starts_with('/') && !target.starts_with("//"),
        "Invalid callback target"
    );
    anyhow::ensure!(
        matches!(start.next(), Some("HTTP/1.1" | "HTTP/1.0")) && start.next().is_none(),
        "Invalid callback request"
    );
    let mut host = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("Invalid callback header"))?;
        if key.eq_ignore_ascii_case("host") {
            anyhow::ensure!(host.is_none(), "Duplicate callback host");
            host = Some(value.trim());
        }
        anyhow::ensure!(
            !key.eq_ignore_ascii_case("transfer-encoding"),
            "Callback body is not allowed"
        );
        if key.eq_ignore_ascii_case("content-length") {
            anyhow::ensure!(value.trim() == "0", "Callback body is not allowed");
        }
    }
    let authority = format!("127.0.0.1:{}", redirect.port().unwrap_or(80));
    anyhow::ensure!(host == Some(authority.as_str()), "Wrong callback host");
    let uri = redirect.join(target)?;
    anyhow::ensure!(
        uri.origin() == redirect.origin()
            && uri.path() == redirect.path()
            && uri.fragment().is_none(),
        "Wrong callback path"
    );
    let mut keys = std::collections::HashSet::new();
    for (key, value) in uri.query_pairs() {
        anyhow::ensure!(
            keys.insert(key.to_string()) && value.len() <= 4096,
            "Duplicate or oversized callback parameter"
        );
    }
    anyhow::ensure!(uri.query().is_some(), "Missing callback parameters");
    Ok(uri)
}

pub(super) async fn respond(stream: &mut TcpStream, success: bool) -> anyhow::Result<()> {
    let (status, text) = if success {
        (
            "200 OK",
            "Sign-in completed. You can close this tab and return to Messages.",
        )
    } else {
        (
            "400 Bad Request",
            "This sign-in could not be completed. Return to Messages and try again.",
        )
    };
    let message = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{text}",
        text.len()
    );
    stream.write_all(message.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}
