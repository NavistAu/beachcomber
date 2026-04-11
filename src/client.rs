use crate::protocol::Response;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct Client {
    socket_path: PathBuf,
}

/// A persistent connection to the beachcomber daemon.
/// Reuses a single Unix socket connection across multiple requests,
/// avoiding the per-request connect/disconnect overhead.
pub struct ClientSession {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl ClientSession {
    pub async fn connect(socket_path: &std::path::Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
        })
    }

    pub async fn set_context(&mut self, path: &str) -> std::io::Result<Response> {
        let request = serde_json::json!({ "op": "context", "path": path });
        self.send_request(&request).await
    }

    pub async fn get(&mut self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        let mut request = serde_json::json!({ "op": "get", "key": key });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn store(
        &mut self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        let mut request = serde_json::json!({ "op": "store", "key": key, "data": data });
        if let Some(t) = ttl {
            request["ttl"] = serde_json::json!(t);
        }
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn get_text(&mut self, key: &str, path: Option<&str>) -> std::io::Result<String> {
        let mut request = serde_json::json!({ "op": "get", "key": key, "format": "text" });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.writer.write_all(msg.as_bytes()).await?;
        // Text responses may be multi-line (Object values are key=val per line),
        // terminated by a blank line. Single-value responses are one line.
        let mut result = String::new();
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            if line == "\n" {
                break;
            }
            if result.is_empty() && line.starts_with("error:") {
                return Err(std::io::Error::other(line.trim_end().to_string()));
            }
            result.push_str(&line);
        }
        Ok(result.trim_end_matches('\n').to_string())
    }

    /// Send a watch request. Call read_watch_line() in a loop to receive updates.
    pub async fn watch(
        &mut self,
        key: &str,
        path: Option<&str>,
        format: Option<&str>,
    ) -> std::io::Result<()> {
        let mut request = serde_json::json!({
            "op": "watch",
            "key": key,
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        if let Some(f) = format {
            request["format"] = serde_json::json!(f);
        }
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        self.writer.write_all(msg.as_bytes()).await?;
        Ok(())
    }

    /// Read the next watch update line. Returns None on EOF.
    pub async fn read_watch_line(&mut self) -> std::io::Result<Option<String>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(line))
    }

    async fn send_request(&mut self, request: &serde_json::Value) -> std::io::Result<Response> {
        let msg = format!("{}\n", serde_json::to_string(request).unwrap());
        self.writer.write_all(msg.as_bytes()).await?;
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

impl Client {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Open a persistent session that reuses a single connection for multiple requests.
    pub async fn connect(&self) -> std::io::Result<ClientSession> {
        ClientSession::connect(&self.socket_path).await
    }

    pub async fn get(&self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        let mut request = serde_json::json!({
            "op": "get",
            "key": key,
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn get_text(&self, key: &str, path: Option<&str>) -> std::io::Result<String> {
        let mut request = serde_json::json!({
            "op": "get",
            "key": key,
            "format": "text",
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }

        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
        stream.write_all(msg.as_bytes()).await?;

        let mut reader = BufReader::new(stream);
        let mut result = String::new();
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            if line == "\n" {
                break;
            }
            if result.is_empty() && line.starts_with("error:") {
                return Err(std::io::Error::other(line.trim_end().to_string()));
            }
            result.push_str(&line);
        }
        Ok(result.trim_end_matches('\n').to_string())
    }

    pub async fn store(
        &self,
        key: &str,
        data: serde_json::Value,
        ttl: Option<&str>,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        let mut request = serde_json::json!({ "op": "store", "key": key, "data": data });
        if let Some(t) = ttl {
            request["ttl"] = serde_json::json!(t);
        }
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn poke(&self, key: &str, path: Option<&str>) -> std::io::Result<Response> {
        let mut request = serde_json::json!({
            "op": "poke",
            "key": key,
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn send_raw(&self, request: serde_json::Value) -> std::io::Result<Response> {
        self.send_request(&request).await
    }

    async fn send_request(&self, request: &serde_json::Value) -> std::io::Result<Response> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let msg = format!("{}\n", serde_json::to_string(request).unwrap());
        stream.write_all(msg.as_bytes()).await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
