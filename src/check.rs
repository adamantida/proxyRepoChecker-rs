use std::time::Duration;

pub fn socks_client(port: u16, connect_timeout: Duration) -> anyhow::Result<reqwest::Client> {
    let proxy = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .danger_accept_invalid_certs(true)
        .connect_timeout(connect_timeout)
        .http1_only()
        .build()?;
    Ok(client)
}
