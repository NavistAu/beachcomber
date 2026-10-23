//! HTTP fetch boundary: the one place the daemon makes an HTTP request.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// One request. `method` is GET, POST, PUT or PATCH; anything else is an
/// error. A non-2xx status is a response, not an error: the caller judges it.
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();

    let response = match method {
        "GET" => {
            let mut req = agent.get(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            req.call()
        }
        "POST" | "PUT" | "PATCH" => {
            let mut req = match method {
                "PUT" => agent.put(url),
                "PATCH" => agent.patch(url),
                _ => agent.post(url),
            };
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            let body_bytes = body.unwrap_or_default();
            req.send(body_bytes.as_slice())
        }
        other => {
            return Err(format!(
                "unsupported method {other}: expected GET, POST, PUT or PATCH"
            ));
        }
    };

    let mut response = match response {
        Ok(resp) => resp,
        Err(e) => return Err(e.to_string()),
    };

    let status = response.status().as_u16();

    let body_bytes = response
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;

    Ok(HttpResponse {
        status,
        body: body_bytes,
    })
}
