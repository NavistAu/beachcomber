use crate::protocol::Response;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct Client {
    socket_path: PathBuf,
}

impl Client {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub async fn get(
        &self,
        key: &str,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        let mut request = serde_json::json!({
            "op": "get",
            "key": key,
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn get_text(
        &self,
        key: &str,
        path: Option<&str>,
    ) -> std::io::Result<String> {
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
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let trimmed = line.trim_end_matches('\n').to_string();
        if trimmed.starts_with("error:") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                trimmed,
            ));
        }
        Ok(trimmed)
    }

    pub async fn poke(
        &self,
        key: &str,
        path: Option<&str>,
    ) -> std::io::Result<Response> {
        let mut request = serde_json::json!({
            "op": "poke",
            "key": key,
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        self.send_request(&request).await
    }

    pub async fn send_raw(
        &self,
        request: serde_json::Value,
    ) -> std::io::Result<Response> {
        self.send_request(&request).await
    }

    async fn send_request(
        &self,
        request: &serde_json::Value,
    ) -> std::io::Result<Response> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let msg = format!("{}\n", serde_json::to_string(request).unwrap());
        stream.write_all(msg.as_bytes()).await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        serde_json::from_str(&line).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })
    }
}
