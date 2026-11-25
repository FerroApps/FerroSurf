//! `http_fetch` execution: HTTPS `GET`/`POST` to listed hosts, no redirects,
//! no cookies, bounded time and size. Runs on a short-lived thread.

use std::time::Duration;

use ferrosurf_plugin_sdk::abi::{HttpRequest, HttpResponse};
use url::Url;

/// Largest response body handed to a plugin.
pub const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

fn failed(request_id: u64, msg: impl Into<String>) -> HttpResponse {
    HttpResponse {
        request_id,
        status: 0,
        body: Vec::new(),
        error: Some(msg.into()),
    }
}

/// Performs `req`. The allowlist is checked again here, right before the
/// request leaves.
pub fn fetch(request_id: u64, req: &HttpRequest, hosts: &[String]) -> HttpResponse {
    let Ok(url) = Url::parse(&req.url) else {
        return failed(request_id, "bad url");
    };
    let host_ok = url.scheme() == "https"
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|h| hosts.iter().any(|a| a == &h.to_ascii_lowercase()));
    if !host_ok {
        return failed(request_id, "host not allowed");
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let result = if req.method == "POST" {
        let mut b = agent.post(url.as_str());
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        b.send(&req.body[..])
    } else {
        let mut b = agent.get(url.as_str());
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        b.call()
    };
    match result {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            match resp
                .body_mut()
                .with_config()
                .limit(MAX_RESPONSE_BYTES)
                .read_to_vec()
            {
                Ok(body) => HttpResponse {
                    request_id,
                    status,
                    body,
                    error: None,
                },
                Err(e) => failed(request_id, e.to_string()),
            }
        }
        Err(e) => failed(request_id, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_unlisted_hosts_without_touching_the_network() {
        let r = fetch(
            3,
            &HttpRequest {
                method: "GET".into(),
                url: "https://evil.example/".into(),
                ..Default::default()
            },
            &["api.example.com".into()],
        );
        assert_eq!((r.request_id, r.status), (3, 0));
        assert_eq!(r.error.as_deref(), Some("host not allowed"));
    }
}
