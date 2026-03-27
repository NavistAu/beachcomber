use crate::protocol::{Format, Response};
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
        format: Format,
    ) -> std::io::Result<Response> {
        let mut request = serde_json::json!({
            "op": "get",
            "key": key,
        });
        if let Some(p) = path {
            request["path"] = serde_json::json!(p);
        }
        if format == Format::Text {
            request["format"] = serde_json::json!("text");
        }
        self.send_request(&request, format == Format::Text).await
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

        Ok(line.trim_end_matches('\n').to_string())
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
        self.send_request(&request, false).await
    }

    async fn send_request(
        &self,
        request: &serde_json::Value,
        _text_mode: bool,
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
