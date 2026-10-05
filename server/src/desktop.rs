//! Calls to Desktop's own HTTP API (DIMOS_APP's `desktopUrl`, e.g. `http://127.0.0.1:7077`): a plain HTTP/1.1 request on
//! loopback, enough for its JSON endpoints, so the server needs no HTTP client library.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOO_OLD: &str = "uploading needs a newer dimOS Desktop (one with cloud uploads)";

pub async fn post(base: &str, path: &str, body: &Value) -> Result<Value> {
    let rest = base.strip_prefix("http://").context("Desktop's URL must be http://host:port")?;
    let host = rest.trim_end_matches('/').split('/').next().unwrap_or_default().to_string();
    let address = if host.contains(':') { host.clone() } else { format!("{host}:80") };
    let payload = body.to_string();
    let request = format!("POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len());
    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::TcpStream::connect(&address)).await.context("Desktop didn't answer")?.with_context(|| format!("can't reach Desktop at {address}"))?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(30), stream.read_to_end(&mut raw)).await.context("Desktop didn't answer")??;
    let (status, body) = parse_response(&raw)?;
    if status == 404 || status == 405 {
        bail!(TOO_OLD);
    }
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        bail!("{}", value["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("Desktop said {status}")));
    }
    Ok(value)
}

/// (status, body) of an HTTP/1.1 response, with a chunked body put back together.
fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").context("not an HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status: u16 = head.split_whitespace().nth(1).and_then(|code| code.parse().ok()).context("no HTTP status")?;
    let mut body = raw[split + 4..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        let mut joined = Vec::new();
        let mut rest = &body[..];
        while let Some(end) = rest.windows(2).position(|w| w == b"\r\n") {
            let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..end]).trim(), 16).context("chunk size")?;
            if size == 0 {
                break;
            }
            let start = end + 2;
            joined.extend_from_slice(rest.get(start..start + size).context("short chunk")?);
            rest = rest.get(start + size + 2..).unwrap_or_default();
        }
        body = joined;
    }
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_chunked_bodies() {
        let (status, body) = parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").unwrap();
        assert_eq!((status, body), (200, b"{}".to_vec()));
        let (status, body) = parse_response(b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n").unwrap();
        assert_eq!((status, body), (400, b"{\"a\":1}".to_vec()));
    }

    #[tokio::test]
    async fn errors_are_readable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = socket.read(&mut buffer).await;
            socket.write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 25\r\n\r\n{\"error\":\"not an .mcap\"}").await.unwrap();
        });
        let error = post(&format!("http://{address}"), "/dimos/uploads", &serde_json::json!({ "path": "x" })).await.unwrap_err();
        assert_eq!(error.to_string(), "not an .mcap");
    }
}
