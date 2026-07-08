//! DopeDB stdio ↔ TCP bridge.
//!
//! Claude Desktop (and any stdio-only MCP client) can't dial a localhost HTTP server,
//! so it spawns THIS binary as its MCP "command". The bridge reads the running app's
//! bridge port + token from `mcp.json`, connects to the app's local TCP MCP listener,
//! authenticates with the token (first line), then pumps bytes both ways. It contains
//! ZERO MCP logic — all tools/safety live in the app.
//!
//! The DopeDB app MUST be running; otherwise the TCP connect fails and the bridge
//! exits with a clear message (which the client surfaces).

use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let path = dirs::data_dir()
        .ok_or_else(|| std::io::Error::other("no OS data dir"))?
        .join("dopedb")
        .join("mcp.json");

    let raw = std::fs::read_to_string(&path).map_err(|e| {
        std::io::Error::other(format!(
            "DopeDB is not set up (missing {}): {e}. Open the DopeDB app first.",
            path.display()
        ))
    })?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| std::io::Error::other(format!("bad mcp.json: {e}")))?;
    let token = v["token"].as_str().unwrap_or_default().to_string();
    let port = v["bridgePort"].as_u64().unwrap_or(7687) as u16;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| {
            std::io::Error::other(format!(
                "cannot reach the DopeDB app on 127.0.0.1:{port} ({e}). Is the app running?"
            ))
        })?;

    // Authenticate: the first line is the bearer token.
    stream.write_all(format!("{token}\n").as_bytes()).await?;

    let (mut server_r, mut server_w) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Pump both directions. On stdin EOF, shut down the write half so the server sees
    // EOF and closes its side (otherwise the down-stream copy would hang forever).
    let up = async {
        tokio::io::copy(&mut stdin, &mut server_w).await?;
        server_w.shutdown().await?;
        Ok::<(), std::io::Error>(())
    };
    let down = async {
        tokio::io::copy(&mut server_r, &mut stdout).await?;
        stdout.flush().await?;
        Ok::<(), std::io::Error>(())
    };
    tokio::try_join!(up, down)?;
    Ok(())
}
