//! Shared synchronous HTTP client (`ureq` + `rustls`, zero OpenSSL dependency),
//! loopback Connect-RPC helper, and offline JWT payload decoder.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use std::time::Duration;
use ureq::tls::TlsConfig;
use ureq::Agent;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Optional URL overrides used by hermetic `127.0.0.1` mock-HTTP integration tests.
#[derive(Debug, Default, Clone)]
pub struct EndpointOverrides {
    pub claude_usage_url: Option<String>,
    pub codex_wham_url: Option<String>,
    pub grok_billing_url: Option<String>,
    pub cursor_usage_url: Option<String>,
    pub agy_rpc_url: Option<String>,
}

#[derive(Clone)]
pub struct HttpClient {
    agent: Agent,
    loopback_agent: Agent,
}

impl HttpClient {
    pub fn new(timeout: Duration) -> Self {
        let clamped = timeout.clamp(Duration::from_secs(2), Duration::from_secs(60));
        let connect_timeout = Duration::from_secs(5).min(clamped);

        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(connect_timeout))
            .timeout_global(Some(clamped))
            .build();

        let loopback_config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_millis(500)))
            .timeout_global(Some(Duration::from_secs(3).min(clamped)))
            .tls_config(TlsConfig::builder().disable_verification(true).build())
            .build();

        Self {
            agent: config.into(),
            loopback_agent: loopback_config.into(),
        }
    }

    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        let mut req = self.agent.get(url);
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .call()
            .with_context(|| format!("HTTP GET failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }

    pub fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        json_body: &str,
    ) -> Result<HttpResponse> {
        let mut req = self
            .agent
            .post(url)
            .header("Content-Type", "application/json");
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .send(json_body.as_bytes())
            .with_context(|| format!("HTTP POST failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }

    pub fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        form_body: &str,
    ) -> Result<HttpResponse> {
        let mut req = self
            .agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded");
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .send(form_body.as_bytes())
            .with_context(|| format!("HTTP POST form failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }

    /// Sends a Connect-RPC JSON POST strictly to a `127.0.0.1` loopback endpoint.
    pub fn post_loopback_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        json_body: &str,
    ) -> Result<HttpResponse> {
        if !url.starts_with("http://127.0.0.1:") && !url.starts_with("https://127.0.0.1:") {
            return Err(anyhow!(
                "Refusing loopback RPC to non-loopback address: {}",
                redact_url(url)
            ));
        }

        let mut req = self
            .loopback_agent
            .post(url)
            .header("Content-Type", "application/json");
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .send(json_body.as_bytes())
            .with_context(|| format!("Loopback POST failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }
}

fn read_http_response(resp: &mut ureq::http::Response<ureq::Body>) -> Result<HttpResponse> {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    let body = resp
        .body_mut()
        .read_to_string()
        .context("Failed to read HTTP response body")?;

    Ok(HttpResponse {
        status,
        retry_after,
        body,
    })
}

fn redact_url(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Offline base64url decoder for extracting non-secret identity/plan claims (`email`, `exp`, etc.)
/// from a JWT payload without network I/O.
pub fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let trimmed = token.trim();
    let mut parts = trimmed.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _sig = parts.next()?;

    let bytes = URL_SAFE_NO_PAD
        .decode(payload_b64.trim_end_matches('='))
        .or_else(|_| URL_SAFE.decode(payload_b64))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn test_decode_jwt_payload() {
        let payload = r#"{"email":"dev@example.com","exp":1794380000,"https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}}"#;
        let encoded = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        let fake_jwt = format!("eyJhbGciOiJSUzI1NiJ9.{encoded}.signature");

        let claims = decode_jwt_payload(&fake_jwt).expect("decode jwt");
        assert_eq!(claims["email"], "dev@example.com");
        assert_eq!(claims["exp"], 1794380000);
        assert_eq!(
            claims["https://api.openai.com/auth"]["chatgpt_plan_type"],
            "pro"
        );
    }

    #[test]
    fn test_http_client_handles_429_and_retry_after_without_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 1024];
            let _ = stream.read(&mut req_buf);
            let resp = concat!(
                "HTTP/1.1 429 Too Many Requests\r\n",
                "Retry-After: 0\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: 29\r\n",
                "Connection: close\r\n\r\n",
                "{\"error\":\"rate_limit_error\"}\n"
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let client = HttpClient::new(Duration::from_secs(5));
        let url = format!("http://{addr}/api/oauth/usage");
        let res = client
            .get(&url, &[("User-Agent", "claude-code/2.1.76")])
            .expect("429 should return HttpResponse, not transport error");
        handle.join().unwrap();

        assert_eq!(res.status, 429);
        assert_eq!(res.retry_after, Some(0));
        assert!(res.body.contains("rate_limit_error"));
    }

    #[test]
    fn test_loopback_guard_rejects_remote_host() {
        let client = HttpClient::new(Duration::from_secs(2));
        let err = client
            .post_loopback_json("https://example.com/rpc", &[], "{}")
            .unwrap_err();
        assert!(err.to_string().contains("Refusing loopback RPC"));
    }
}
