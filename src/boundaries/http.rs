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
        _method: String,
        _url: String,
        _headers: Vec<(String, String)>,
        _body: Option<Vec<u8>>,
        _timeout: Duration,
    ) -> Result<HttpResponse, String> {
        // Real impl is moved here from src/provider/http.rs in P3.7.
        todo!("move ureq calls from provider/http.rs in P3.7")
    }
}
