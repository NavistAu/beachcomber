//! HTTP fetcher boundary trait.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[cfg_attr(test, mockall::automock)]
pub trait HttpFetcher: Send + Sync {
    fn fetch(
        &self,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<HttpResponse, String>;
}

pub struct UreqHttpFetcher;

impl HttpFetcher for UreqHttpFetcher {
    fn fetch(
        &self,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<HttpResponse, String> {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(timeout))
            .build();
        let agent: ureq::Agent = config.into();

        let response = match method.as_str() {
            "POST" | "PUT" | "PATCH" => {
                let mut req = match method.as_str() {
                    "PUT" => agent.put(&url),
                    "PATCH" => agent.patch(&url),
                    _ => agent.post(&url),
                };
                for (key, val) in &headers {
                    req = req.header(key.as_str(), val.as_str());
                }
                let body_bytes = body.unwrap_or_default();
                req.send(body_bytes.as_slice())
            }
            _ => {
                let mut req = agent.get(&url);
                for (key, val) in &headers {
                    req = req.header(key.as_str(), val.as_str());
                }
                req.call()
            }
        };

        let mut response = match response {
            Ok(resp) => resp,
            Err(e) => return Err(e.to_string()),
        };

        let resp_headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        let status = response.status().as_u16();

        let body_bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|e| e.to_string())?;

        Ok(HttpResponse {
            status,
            headers: resp_headers,
            body: body_bytes,
        })
    }
}
