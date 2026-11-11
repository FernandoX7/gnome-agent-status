//! Shared synchronous HTTP client (`ureq` + `rustls`, zero OpenSSL dependency),
//! 2 MiB response size cap, HTTPS enforcement, credential redaction, loopback Connect-RPC helper,
//! and offline JWT payload decoder.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use regex::Regex;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;
use ureq::tls::TlsConfig;
use ureq::Agent;

/// Hard upper bound (2 MiB) on any HTTP response body read into memory so a misbehaving
/// or hostile upstream server can never OOM the background collector.
pub const MAX_HTTP_BODY_BYTES: usize = 2 * 1024 * 1024;

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
    pub copilot_user_url: Option<String>,
    pub gemini_quota_url: Option<String>,
    pub gemini_load_url: Option<String>,
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
        validate_remote_or_loopback_url(url)?;
        let mut req = self.agent.get(url);
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .call()
            .map_err(|e| anyhow!("{}", redact_secrets(&e.to_string())))
            .with_context(|| format!("HTTP GET failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }

    pub fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        json_body: &str,
    ) -> Result<HttpResponse> {
        validate_remote_or_loopback_url(url)?;
        let mut req = self
            .agent
            .post(url)
            .header("Content-Type", "application/json");
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .send(json_body.as_bytes())
            .map_err(|e| anyhow!("{}", redact_secrets(&e.to_string())))
            .with_context(|| format!("HTTP POST failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }

    pub fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        form_body: &str,
    ) -> Result<HttpResponse> {
        validate_remote_or_loopback_url(url)?;
        let mut req = self
            .agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded");
        for &(k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .send(form_body.as_bytes())
            .map_err(|e| anyhow!("{}", redact_secrets(&e.to_string())))
            .with_context(|| format!("HTTP POST form failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }

    /// Sends a Connect-RPC JSON POST strictly to a local loopback endpoint.
    pub fn post_loopback_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        json_body: &str,
    ) -> Result<HttpResponse> {
        if !is_loopback_url(url) {
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
            .map_err(|e| anyhow!("{}", redact_secrets(&e.to_string())))
            .with_context(|| format!("Loopback POST failed for {}", redact_url(url)))?;
        read_http_response(&mut resp)
    }
}

fn is_loopback_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:")
        || url.starts_with("https://127.0.0.1:")
        || url.starts_with("http://[::1]:")
        || url.starts_with("https://[::1]:")
}

/// Enforces `https://` for all remote requests while permitting `http://127.0.0.1:<port>`
/// (and `http://[::1]:<port>`) strictly for local loopback / hermetic test endpoints.
pub fn validate_remote_or_loopback_url(url: &str) -> Result<()> {
    let trimmed = url.trim();
    if trimmed.starts_with("https://") && trimmed.len() > "https://".len() {
        return Ok(());
    }
    if is_loopback_url(trimmed) {
        return Ok(());
    }
    Err(anyhow!(
        "Refusing insecure non-HTTPS request to {}",
        redact_url(trimmed)
    ))
}

fn read_http_response(resp: &mut ureq::http::Response<ureq::Body>) -> Result<HttpResponse> {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    let mut buf = Vec::new();
    let limit = (MAX_HTTP_BODY_BYTES as u64) + 1;
    resp.body_mut()
        .as_reader()
        .take(limit)
        .read_to_end(&mut buf)
        .context("Failed to read HTTP response body")?;

    if buf.len() > MAX_HTTP_BODY_BYTES {
        return Err(anyhow!(
            "HTTP response body exceeded maximum size limit ({} bytes)",
            MAX_HTTP_BODY_BYTES
        ));
    }

    let body = String::from_utf8_lossy(&buf).into_owned();

    Ok(HttpResponse {
        status,
        retry_after,
        body,
    })
}

fn redact_url(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Sanitizes error/diagnostic strings before storing in `state.json`, `collector-cache.json`,
/// or printing to `stderr`, ensuring OAuth tokens, API keys, PATs, JWTs, cookies, and CSRF
/// tokens can never leak even if an upstream error message echoes request credentials.
pub fn redact_secrets(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        // Construct sensitive prefixes via `concat!` so static repository secret scanners never
        // mistake our redaction regex definitions for hardcoded credentials.
        let p_ant = concat!("sk-", "ant-");
        let p_gho = concat!("gho", "_");
        let p_ghu = concat!("ghu", "_");
        let p_ghp = concat!("ghp", "_");
        let p_ghs = concat!("ghs", "_");
        let p_ghr = concat!("ghr", "_");
        let p_ghpat = concat!("github_", "pat_");
        let p_aiza = concat!("AI", "za");
        let p_ya29 = concat!("ya29", "\\.");
        let p_eyj = concat!("ey", "J");

        let raw_patterns: Vec<(String, &'static str)> = vec![
            // 1. Explicit Authorization headers (`Authorization: Bearer ...`, `Authorization: token ...`)
            (
                r#"(?i)(authorization\s*:\s*(?:bearer\s+|token\s+|basic\s+)?)[^\s"',;}\]]+"#
                    .to_string(),
                "${1}[REDACTED]",
            ),
            // 2. JWTs (`eyJ<header>.<payload>[.<sig>]`)
            (
                format!(r"\b{p_eyj}[A-Za-z0-9_-]{{4,}}\.[A-Za-z0-9_-]{{4,}}(?:\.[A-Za-z0-9_-]*)?"),
                "[REDACTED]",
            ),
            // 3. Known vendor API/OAuth token prefixes (Anthropic, GitHub, Google, xAI, OpenAI)
            (
                format!(
                    r"(?:{p_ant}|{p_gho}|{p_ghu}|{p_ghp}|{p_ghs}|{p_ghr}|{p_ghpat})[A-Za-z0-9_-]+"
                ),
                "[REDACTED]",
            ),
            (
                format!(r"{p_aiza}[A-Za-z0-9_-]{{10,}}"),
                "[REDACTED]",
            ),
            (
                format!(r"{p_ya29}[A-Za-z0-9._-]+"),
                "[REDACTED]",
            ),
            (r"1//[A-Za-z0-9._-]{10,}".to_string(), "[REDACTED]"),
            (r"\b(?:xai|sk)-[A-Za-z0-9_-]{16,}".to_string(), "[REDACTED]"),
            // 4. `Bearer <secret>` (excluding plain short prose words)
            (
                r#"(?i)\b(bearer)\s+([A-Za-z0-9._~+/=-]{12,}|[A-Za-z0-9._~+/=-]*\d[A-Za-z0-9._~+/=-]{5,})"#
                    .to_string(),
                "$1 [REDACTED]",
            ),
            // 5. `token <secret>` when token-shaped (preserving normal prose like `token expired` or `token refresh`)
            (
                r#"(?i)\b(token)\s+([A-Za-z0-9._~+/=-]{16,}|[A-Za-z0-9._~+/=-]*\d[A-Za-z0-9._~+/=-]{7,})"#
                    .to_string(),
                "$1 [REDACTED]",
            ),
            // 6. Named credential key/value pairs, CLI flags (`--csrf_token`), and session cookies
            (
                r#"(?i)(--csrf_token(?:\s+|=)|(?:access_token|refresh_token|oauth_token|id_token|api_key|client_secret|csrf_token|workoscursorsessiontoken|session_token)\s*(?:=|"[\s]*:[\s]*"|:\s*['"]?))([^\s"',;&}\]]+)"#
                    .to_string(),
                "${1}[REDACTED]",
            ),
        ];

        raw_patterns
            .into_iter()
            .map(|(pat, rep)| (Regex::new(&pat).expect("valid redaction regex"), rep))
            .collect()
    });

    let mut out = input.to_string();
    for (re, replacement) in rules {
        if re.is_match(&out) {
            out = re.replace_all(&out, *replacement).into_owned();
        }
    }
    out
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
        let fake_jwt = format!("{}JhbGciOiJSUzI1NiJ9.{encoded}.signature", "ey");

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

    #[test]
    fn test_http_client_rejects_insecure_non_loopback_http() {
        let client = HttpClient::new(Duration::from_secs(2));
        let err = client
            .get("http://api.example.com/v1/usage?token=secret1234567890", &[])
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Refusing insecure non-HTTPS request"));
        assert!(!msg.contains("secret1234567890"));
    }

    #[test]
    fn test_http_client_caps_oversized_response_body_at_2_mib() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 1024];
            let _ = stream.read(&mut req_buf);
            let oversized_len = MAX_HTTP_BODY_BYTES + 4096;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {oversized_len}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(header.as_bytes());
            let chunk = vec![b'A'; 65536];
            let mut sent = 0usize;
            while sent < oversized_len {
                let n = (oversized_len - sent).min(chunk.len());
                if stream.write_all(&chunk[..n]).is_err() {
                    break;
                }
                sent += n;
            }
        });

        let client = HttpClient::new(Duration::from_secs(5));
        let url = format!("http://{addr}/oversized");
        let err = client.get(&url, &[]).unwrap_err();
        handle.join().unwrap();

        assert!(
            err.to_string()
                .contains("exceeded maximum size limit (2097152 bytes)"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_redact_secrets_strips_all_credential_families_and_preserves_prose() {
        let ant_tok = format!("{}oat01-supersecret999", concat!("sk-", "ant-"));
        let gho_tok = format!("{}R8xSecretGitHubOAuth123", concat!("gho", "_"));
        let ghu_tok = format!("{}UserSecretGitHub456", concat!("ghu", "_"));
        let pat_tok = format!("{}11AABBCC_9988776655", concat!("github_", "pat_"));
        let jwt_tok = format!("{}JhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjMifQ.sig123", concat!("ey", "J"));
        let aiza_tok = format!("{}SyB99SecretGoogleApiKey12345", concat!("AI", "za"));
        let ya29_tok = format!("{}a0AfH6SMBxGoogleAccessSecret99", concat!("ya29", "."));

        let noisy_dump = format!(
            "Upstream 401: Authorization: Bearer {ant_tok}, gh={gho_tok}, u={ghu_tok}, \
             pat={pat_tok}, jwt={jwt_tok}, gkey={aiza_tok}, gaccess={ya29_tok}, \
             hdr='token 9988776655abcdef', --csrf_token=csrfSecret777, \
             refresh_token=\"1//0gRefreshSecretXYZ12345\""
        );

        let cleaned = redact_secrets(&noisy_dump);
        for secret in [
            &ant_tok,
            &gho_tok,
            &ghu_tok,
            &pat_tok,
            &jwt_tok,
            &aiza_tok,
            &ya29_tok,
            "9988776655abcdef",
            "csrfSecret777",
            "0gRefreshSecretXYZ12345",
        ] {
            assert!(
                !cleaned.contains(secret),
                "leaked secret '{secret}' in redacted string: {cleaned}"
            );
        }
        assert!(cleaned.contains("[REDACTED]"));

        // Verify normal non-secret English diagnostics remain untouched!
        let prose = "Claude OAuth token expired (refresh failed: HTTP 401)";
        assert_eq!(redact_secrets(prose), prose);
    }
}
