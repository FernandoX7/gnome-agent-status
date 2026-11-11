//! End-to-end hermetic integration tests using a local `127.0.0.1` mock HTTP server,
//! mock credential trees, and full CLI subcommand execution.

use crate::cache::CacheStore;
use crate::cli::{execute_cli, Cli, CliRuntime};
use crate::config::ConfigShowSummary;
use crate::http::{EndpointOverrides, HttpClient};
use crate::model::{AccountState, OverallStatus, StatusReport};
use crate::providers::FetchContext;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{TimeZone, Utc};
use clap::Parser;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn unique_sandbox(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gas-e2e-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_jwt(payload_json: &str) -> String {
    let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let body = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    format!("{hdr}.{body}.sig")
}

struct MockApiServer {
    base_url: String,
    stop: Arc<AtomicBool>,
    claude_hits: Arc<AtomicUsize>,
    codex_hits: Arc<AtomicUsize>,
    grok_hits: Arc<AtomicUsize>,
    agy_hits: Arc<AtomicUsize>,
    cursor_hits: Arc<AtomicUsize>,
    copilot_hits: Arc<AtomicUsize>,
    gemini_hits: Arc<AtomicUsize>,
    rate_limit_claude: Arc<AtomicBool>,
    copilot_unsubscribed: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockApiServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on mock listener");
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");

        let stop = Arc::new(AtomicBool::new(false));
        let claude_hits = Arc::new(AtomicUsize::new(0));
        let codex_hits = Arc::new(AtomicUsize::new(0));
        let grok_hits = Arc::new(AtomicUsize::new(0));
        let agy_hits = Arc::new(AtomicUsize::new(0));
        let cursor_hits = Arc::new(AtomicUsize::new(0));
        let copilot_hits = Arc::new(AtomicUsize::new(0));
        let gemini_hits = Arc::new(AtomicUsize::new(0));
        let rate_limit_claude = Arc::new(AtomicBool::new(false));
        let copilot_unsubscribed = Arc::new(AtomicBool::new(false));

        let stop_bg = Arc::clone(&stop);
        let c_hits = Arc::clone(&claude_hits);
        let x_hits = Arc::clone(&codex_hits);
        let g_hits = Arc::clone(&grok_hits);
        let a_hits = Arc::clone(&agy_hits);
        let u_hits = Arc::clone(&cursor_hits);
        let p_hits = Arc::clone(&copilot_hits);
        let m_hits = Arc::clone(&gemini_hits);
        let rl_claude = Arc::clone(&rate_limit_claude);
        let cp_unsub = Arc::clone(&copilot_unsubscribed);

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        handle_mock_http(
                            stream,
                            &MockHitCounters {
                                claude: &c_hits,
                                codex: &x_hits,
                                grok: &g_hits,
                                agy: &a_hits,
                                cursor: &u_hits,
                                copilot: &p_hits,
                                gemini: &m_hits,
                                rate_limit_claude: &rl_claude,
                                copilot_unsubscribed: &cp_unsub,
                            },
                        );
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url,
            stop,
            claude_hits,
            codex_hits,
            grok_hits,
            agy_hits,
            cursor_hits,
            copilot_hits,
            gemini_hits,
            rate_limit_claude,
            copilot_unsubscribed,
            handle: Some(handle),
        }
    }
}

struct MockHitCounters<'a> {
    claude: &'a AtomicUsize,
    codex: &'a AtomicUsize,
    grok: &'a AtomicUsize,
    agy: &'a AtomicUsize,
    cursor: &'a AtomicUsize,
    copilot: &'a AtomicUsize,
    gemini: &'a AtomicUsize,
    rate_limit_claude: &'a AtomicBool,
    copilot_unsubscribed: &'a AtomicBool,
}

impl Drop for MockApiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn handle_mock_http(mut stream: TcpStream, counters: &MockHitCounters<'_>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });

    let mut req_line = String::new();
    if reader.read_line(&mut req_line).is_err() || req_line.is_empty() {
        return;
    }
    let parts: Vec<&str> = req_line.split_whitespace().collect();
    let path = parts.get(1).copied().unwrap_or("/");

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_len: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "content-length" {
                content_len = val.parse().unwrap_or(0);
            }
            headers.push((key, val));
        }
    }

    if content_len > 0 {
        let mut body_buf = vec![0u8; content_len];
        let _ = reader.read_exact(&mut body_buf);
    }

    let get_hdr = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    let (status_line, extra_hdrs, body) = if path.starts_with("/api/oauth/usage") {
        counters.claude.fetch_add(1, Ordering::SeqCst);

        // Enforce required Anthropic OAuth headers (`claude-code/<ver>` UA + `anthropic-beta`)!
        let ua_ok = get_hdr("user-agent").is_some_and(|ua| ua.starts_with("claude-code/"));
        let beta_ok = get_hdr("anthropic-beta").is_some_and(|b| b.contains("oauth-2025-04-20"));
        if !ua_ok || !beta_ok {
            (
                "HTTP/1.1 429 Too Many Requests",
                "Retry-After: 0\r\n",
                r#"{"error":{"type":"rate_limit_error","message":"missing required oauth headers"}}"#
                    .to_string(),
            )
        } else if counters.rate_limit_claude.load(Ordering::SeqCst) {
            // Simulate Anthropic's misleading `Retry-After: 0` edge rate-limit trap
            (
                "HTTP/1.1 429 Too Many Requests",
                "Retry-After: 0\r\n",
                r#"{"error":{"type":"rate_limit_error","message":"Number of request has exceeded rate limit"}}"#
                    .to_string(),
            )
        } else {
            let auth = get_hdr("authorization").unwrap_or_default();
            let wk_pct = if auth.contains("tok-work") { 89.0 } else { 24.0 };
            (
                "HTTP/1.1 200 OK",
                "",
                format!(
                    r#"{{
                        "five_hour": {{"utilization": 37.0, "resets_at": "2026-11-11T04:00:00Z"}},
                        "seven_day": {{"utilization": {wk_pct}, "resets_at": "2026-11-12T14:00:00Z"}},
                        "seven_day_fable": {{"utilization": 46.0, "resets_at": "2026-11-12T14:00:00Z"}},
                        "extra_usage": {{"is_enabled": true, "monthly_limit": 5000, "used_credits": 420.0, "utilization": 8.4}}
                    }}"#
                ),
            )
        }
    } else if path.starts_with("/backend-api/wham/usage") {
        counters.codex.fetch_add(1, Ordering::SeqCst);
        let acct_hdr = get_hdr("chatgpt-account-id").unwrap_or_default();
        if acct_hdr != "acct_team_77" {
            (
                "HTTP/1.1 400 Bad Request",
                "",
                r#"{"error":"missing ChatGPT-Account-Id"}"#.to_string(),
            )
        } else {
            (
                "HTTP/1.1 200 OK",
                "",
                r#"{
                    "email": "codex@team.dev",
                    "plan_type": "pro",
                    "rate_limit": {
                        "primary_window": {"used_percent": 19, "limit_window_seconds": 18000, "reset_at": 1794375600},
                        "secondary_window": {"used_percent": 35, "limit_window_seconds": 604800, "reset_at": 1794941186}
                    },
                    "credits": {"has_credits": true, "unlimited": false, "balance": "42.50"}
                }"#
                .to_string(),
            )
        }
    } else if path.starts_with("/v1/billing") {
        counters.grok.fetch_add(1, Ordering::SeqCst);
        // Omit `creditUsagePercent` intentionally to exercise protobuf default-zero omission (`0%`)!
        (
            "HTTP/1.1 200 OK",
            "",
            r#"{
                "config": {
                    "currentPeriod": {
                        "type": "USAGE_PERIOD_TYPE_WEEKLY",
                        "start": "2026-11-04T15:13:00Z",
                        "end": "2026-11-11T15:13:00Z"
                    },
                    "prepaidBalance": {"val": 15.0}
                }
            }"#
            .to_string(),
        )
    } else if path.contains("GetUserStatus") {
        counters.agy.fetch_add(1, Ordering::SeqCst);
        let csrf = get_hdr("x-codeium-csrf-token").unwrap_or_default();
        if csrf != "csrf-e2e-token" {
            (
                "HTTP/1.1 403 Forbidden",
                "",
                r#"{"error":"invalid csrf"}"#.to_string(),
            )
        } else {
            (
                "HTTP/1.1 200 OK",
                "",
                r#"{
                    "userStatus": {
                        "email": "agy@google.dev",
                        "planStatus": {"planInfo": {"planName": "Google AI Ultra"}},
                        "cascadeModelConfigData": {
                            "clientModelConfigs": [
                                {"label": "Gemini 3.1 Pro", "quotaInfo": {"remainingFraction": 0.84, "resetTime": "2026-11-12T15:56:12Z"}},
                                {"label": "Claude Sonnet 4.6", "quotaInfo": {"remainingFraction": 0.75, "resetTime": "2026-11-14T12:00:00Z"}}
                            ]
                        }
                    }
                }"#
                .to_string(),
            )
        }
    } else if path.contains("GetCurrentPeriodUsage") {
        counters.cursor.fetch_add(1, Ordering::SeqCst);
        (
            "HTTP/1.1 200 OK",
            "",
            r#"{
                "billingCycleEnd": "2026-12-01T00:00:00Z",
                "membershipType": "ultra",
                "planUsage": {
                    "totalPercentUsed": 12.0,
                    "autoPercentUsed": 16.0,
                    "apiPercentUsed": 11.0,
                    "used": 4800,
                    "limit": 40000
                }
            }"#
            .to_string(),
        )
    } else if path.starts_with("/copilot_internal/user") {
        counters.copilot.fetch_add(1, Ordering::SeqCst);
        let auth_ok = get_hdr("authorization").is_some_and(|a| a.starts_with("token "));
        let editor_ok = get_hdr("editor-version").is_some_and(|e| e.starts_with("vscode/"));
        let ua_ok = get_hdr("user-agent").is_some_and(|u| u.starts_with("GitHubCopilotChat/"));
        if !auth_ok || !editor_ok || !ua_ok {
            (
                "HTTP/1.1 401 Unauthorized",
                "",
                r#"{"message":"Bad credentials"}"#.to_string(),
            )
        } else if counters.copilot_unsubscribed.load(Ordering::SeqCst) {
            (
                "HTTP/1.1 404 Not Found",
                "",
                r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest"}"#
                    .to_string(),
            )
        } else {
            (
                "HTTP/1.1 200 OK",
                "",
                r#"{
                    "login": "octodev",
                    "copilot_plan": "individual",
                    "quota_reset_date_utc": "2026-12-01T00:00:00Z",
                    "quota_snapshots": {
                        "premium_interactions": {
                            "entitlement": 300,
                            "remaining": 258,
                            "percent_remaining": 86.0,
                            "unlimited": false
                        },
                        "chat": {
                            "entitlement": 0,
                            "remaining": 0,
                            "percent_remaining": 100.0,
                            "unlimited": true
                        }
                    }
                }"#
                .to_string(),
            )
        }
    } else if path.contains("loadCodeAssist") {
        (
            "HTTP/1.1 200 OK",
            "",
            r#"{
                "cloudaicompanionProject": "gem-proj-e2e",
                "paidTier": {"id": "pro-tier", "name": "Gemini Code Assist Pro"}
            }"#
            .to_string(),
        )
    } else if path.contains("retrieveUserQuota") {
        counters.gemini.fetch_add(1, Ordering::SeqCst);
        let auth_ok = get_hdr("authorization").is_some_and(|a| a.starts_with("Bearer "));
        if !auth_ok {
            (
                "HTTP/1.1 401 Unauthorized",
                "",
                r#"{"error":{"code":401,"message":"UNAUTHENTICATED"}}"#.to_string(),
            )
        } else {
            (
                "HTTP/1.1 200 OK",
                "",
                r#"{
                    "buckets": [
                        {"modelId": "gemini-2.5-pro", "tokenType": "REQUESTS", "remainingFraction": 0.78, "resetTime": "2026-11-11T08:00:00Z"},
                        {"modelId": "gemini-2.5-flash", "tokenType": "REQUESTS", "remainingFraction": 0.91, "resetTime": "2026-11-11T08:00:00Z"}
                    ]
                }"#
                .to_string(),
            )
        }
    } else {
        ("HTTP/1.1 404 Not Found", "", "{}".to_string())
    };

    let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_hdrs}\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn setup_mock_home_and_proc(sandbox: &Path, agy_port: u16) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let home = sandbox.join("home");
    let xdg_cfg = home.join(".config");
    let proc_root = sandbox.join("proc");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg_cfg).unwrap();

    // 1. Claude: default `~/.claude` + manual `~/.claude-work` (2 distinct accounts)
    let claude_def = home.join(".claude");
    fs::create_dir_all(&claude_def).unwrap();
    fs::write(
        claude_def.join(".credentials.json"),
        r#"{
            "claudeAiOauth": {
                "accessToken": "mock-oat01-tok-personal",
                "expiresAt": 4102444800000,
                "subscriptionType": "pro"
            }
        }"#,
    )
    .unwrap();
    fs::write(
        claude_def.join(".claude.json"),
        r#"{"oauthAccount": {"accountUuid": "uuid-personal", "emailAddress": "personal@home.io"}}"#,
    )
    .unwrap();

    let claude_work = home.join(".claude-work");
    fs::create_dir_all(&claude_work).unwrap();
    fs::write(
        claude_work.join(".credentials.json"),
        r#"{
            "claudeAiOauth": {
                "accessToken": "mock-oat01-tok-work",
                "expiresAt": 4102444800000,
                "subscriptionType": "max"
            }
        }"#,
    )
    .unwrap();
    fs::write(
        claude_work.join(".claude.json"),
        r#"{"oauthAccount": {"accountUuid": "uuid-work", "emailAddress": "work@corp.dev"}}"#,
    )
    .unwrap();

    // 2. Codex: `~/.codex/auth.json` with JWT containing `chatgpt_account_id`
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let codex_jwt = make_jwt(
        r#"{
            "email": "codex@team.dev",
            "exp": 4102444800,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_account_id": "acct_team_77"
            }
        }"#,
    );
    fs::write(
        codex_dir.join("auth.json"),
        format!(
            r#"{{
                "auth_mode": "chatgpt",
                "tokens": {{
                    "id_token": "{codex_jwt}",
                    "access_token": "{codex_jwt}",
                    "account_id": "acct_team_77"
                }}
            }}"#
        ),
    )
    .unwrap();

    // 3. Grok: `~/.grok/auth.json` AND copied switcher slot `~/.grok-swap/auth.json` with identical UUID!
    let grok_jwt = make_jwt(r#"{"sub":"grok-user-99","email":"grok@x.dev","exp":4102444800}"#);
    let grok_auth_json = format!(
        r#"{{
            "https://auth.x.ai::88732d21-6994-4f6a-9f0c-112233445566": {{
                "access_token": "{grok_jwt}",
                "expires_at": 4102444800
            }}
        }}"#
    );
    for dir_name in [".grok", ".grok-swap"] {
        let d = home.join(dir_name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("auth.json"), &grok_auth_json).unwrap();
    }

    // 4. Antigravity: mock `/proc` exposing `language_server_linux_x64 --csrf_token csrf-e2e-token`
    //    listening on `127.0.0.1:<agy_port>` PLUS a hermetic `mock-agy.sh` returning all 4 buckets!
    let pid_dir = proc_root.join("4242");
    let fd_dir = pid_dir.join("fd");
    let net_dir = proc_root.join("net");
    fs::create_dir_all(&fd_dir).unwrap();
    fs::create_dir_all(&net_dir).unwrap();
    fs::write(
        pid_dir.join("cmdline"),
        b"/opt/agy/language_server_linux_x64\0--csrf_token\0csrf-e2e-token\0",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("socket:[77777]", fd_dir.join("5")).unwrap();
    let tcp_table = format!(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
         0: 0100007F:{agy_port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 77777 1 0000000000000000 100 0 0 10 0\n"
    );
    fs::write(net_dir.join("tcp"), tcp_table).unwrap();

    let mock_agy_bin = sandbox.join("mock-agy.sh");
    fs::write(
        &mock_agy_bin,
        r#"#!/bin/sh
cat <<'EOF'
{
  "command": {
    "data": {
      "groups": [
        {
          "name": "Gemini",
          "buckets": [
            {"id": "gemini-weekly", "name": "Gemini Weekly", "window": "weekly", "remaining_fraction": 0.84, "reset_time": "2026-11-12T15:56:12Z"},
            {"id": "gemini-5h", "name": "Gemini 5h", "window": "5h", "remaining_fraction": 0.93, "reset_time": "2026-11-11T03:00:00Z"}
          ]
        },
        {
          "name": "Claude and GPT",
          "buckets": [
            {"id": "3p-weekly", "name": "Claude/GPT Weekly", "window": "weekly", "remaining_fraction": 1.0, "reset_time": "2026-11-14T12:00:00Z"},
            {"id": "3p-5h", "name": "Claude/GPT 5h", "window": "5h", "remaining_fraction": 0.75, "reset_time": "2026-11-11T03:00:00Z"}
          ]
        }
      ]
    }
  }
}
EOF
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mock_agy_bin, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // 5. Cursor: JSON token file in manual path
    let cursor_tok_path = home.join("cursor-token.json");
    let cursor_jwt = make_jwt(r#"{"sub":"auth0|cursor-1","email":"cursor@ide.dev","exp":4102444800}"#);
    fs::write(
        &cursor_tok_path,
        format!(r#"{{"accessToken": "{cursor_jwt}"}}"#),
    )
    .unwrap();

    // 6. GitHub Copilot: `~/.config/github-copilot/hosts.json`
    let copilot_dir = xdg_cfg.join("github-copilot");
    fs::create_dir_all(&copilot_dir).unwrap();
    fs::write(
        copilot_dir.join("hosts.json"),
        r#"{"github.com":{"user":"octodev","oauth_token":"mock-ghu-copilot-e2e"}}"#,
    )
    .unwrap();

    // 7. Gemini CLI: `~/.gemini/oauth_creds.json` + `google_accounts.json`
    let gemini_dir = home.join(".gemini");
    fs::create_dir_all(&gemini_dir).unwrap();
    let gemini_id_jwt = make_jwt(r#"{"email":"gem@cli.dev","sub":"gem-123"}"#);
    fs::write(
        gemini_dir.join("oauth_creds.json"),
        format!(
            r#"{{
                "access_token": "mock-ya29-gemini-e2e",
                "refresh_token": "mock-1//refresh-e2e",
                "expiry_date": 4102444800000,
                "id_token": "{gemini_id_jwt}"
            }}"#
        ),
    )
    .unwrap();
    fs::write(
        gemini_dir.join("google_accounts.json"),
        r#"{"active":"gem@cli.dev","old":[]}"#,
    )
    .unwrap();

    (home, xdg_cfg, proc_root, mock_agy_bin)
}

#[test]
fn test_all_7_providers_cli_workflow_dedup_429_decay_switch_and_hook() {
    let sandbox = unique_sandbox("full-suite");
    let server = MockApiServer::start();
    let port: u16 = server
        .base_url
        .rsplit_once(':')
        .unwrap()
        .1
        .parse()
        .unwrap();

    let (home, xdg_cfg, proc_root, mock_agy_bin) = setup_mock_home_and_proc(&sandbox, port);
    let config_path = sandbox.join("config.toml");
    let cache_dir = sandbox.join("cache");
    let switch_marker = sandbox.join("switched-account.txt");

    // Step 1: `--init-config` creates 0600 sample config
    let init_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "--init-config",
    ]);
    let cache_store = CacheStore::new(cache_dir.clone());
    let base_now = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();

    execute_cli(
        &init_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: None,
            now: base_now,
            stdin_data: None,
        },
    )
    .expect("init-config");
    assert!(config_path.exists());

    // Step 2: Use `config upsert-account` to register `claude:work` (with `switch_command`),
    //         `agy:default` (pointing to hermetic `mock-agy.sh`), `cursor:main` (JSON token path),
    //         and temporary `codex:temp` (then `remove-account`!).
    let switch_cmd_str = format!("printf 'work' > '{}'", switch_marker.display());
    let upsert_claude = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "config",
        "upsert-account",
        "--provider",
        "claude",
        "--id",
        "work",
        "--label",
        "Work Claude",
        "--path",
        home.join(".claude-work").to_str().unwrap(),
        "--switch-command",
        &switch_cmd_str,
    ]);
    execute_cli(&upsert_claude, &CliRuntime::default()).expect("upsert claude work");

    let upsert_agy = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "config",
        "upsert-account",
        "--provider",
        "agy",
        "--id",
        "default",
        "--label",
        "Antigravity",
        "--binary",
        mock_agy_bin.to_str().unwrap(),
    ]);
    execute_cli(&upsert_agy, &CliRuntime::default()).expect("upsert agy default");

    let upsert_cursor = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "config",
        "upsert-account",
        "--provider",
        "cursor",
        "--id",
        "main",
        "--label",
        "Cursor Ultra",
        "--path",
        home.join("cursor-token.json").to_str().unwrap(),
    ]);
    execute_cli(&upsert_cursor, &CliRuntime::default()).expect("upsert cursor main");

    let upsert_temp = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "config",
        "upsert-account",
        "--provider",
        "codex",
        "--id",
        "temp",
        "--label",
        "Temp Codex",
    ]);
    execute_cli(&upsert_temp, &CliRuntime::default()).expect("upsert temp");

    let remove_temp = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "config",
        "remove-account",
        "--provider",
        "codex",
        "--id",
        "temp",
    ]);
    let show_json = execute_cli(&remove_temp, &CliRuntime::default())
        .expect("remove temp")
        .expect("config json");
    let summary: ConfigShowSummary = serde_json::from_str(&show_json).expect("parse config show");
    assert_eq!(summary.manual_accounts.len(), 3);
    assert!(summary.manual_accounts.iter().any(|a| a.id == "work"));
    assert!(summary.manual_accounts.iter().any(|a| a.id == "default"));
    assert!(summary.manual_accounts.iter().any(|a| a.id == "main"));

    // Step 3: Run `collect` across all 5 providers concurrently against our local 127.0.0.1 server!
    //         Note `agy_rpc_url: None` so Antigravity exercises real `/proc` -> `fd` -> `net/tcp` discovery!
    let fetch_ctx = FetchContext {
        http: HttpClient::new(Duration::from_secs(5)),
        timeout: Duration::from_secs(5),
        now: base_now,
        home_dir: Some(home.clone()),
        xdg_config_dir: Some(xdg_cfg.clone()),
        proc_root: proc_root.clone(),
        endpoints: EndpointOverrides {
            claude_usage_url: Some(format!("{}/api/oauth/usage", server.base_url)),
            codex_wham_url: Some(format!("{}/backend-api/wham/usage", server.base_url)),
            grok_billing_url: Some(format!("{}/v1/billing?format=credits", server.base_url)),
            agy_rpc_url: None,
            cursor_usage_url: Some(format!(
                "{}/aiserver.v1.DashboardService/GetCurrentPeriodUsage",
                server.base_url
            )),
            copilot_user_url: Some(format!("{}/copilot_internal/user", server.base_url)),
            gemini_quota_url: Some(format!("{}/v1internal:retrieveUserQuota", server.base_url)),
            gemini_load_url: Some(format!("{}/v1internal:loadCodeAssist", server.base_url)),
            ..Default::default()
        },
        env_overrides: Some(std::collections::BTreeMap::new()),
    };

    let collect_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "collect",
        "--pretty",
    ]);
    let collect_out = execute_cli(
        &collect_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: Some(fetch_ctx.clone()),
            now: base_now,
            stdin_data: None,
        },
    )
    .expect("collect")
    .expect("collect stdout");

    let report: StatusReport = serde_json::from_str(&collect_out).expect("parse StatusReport v2");
    assert_eq!(report.schema_version, 2);

    // Canonical 7-provider ordering + all 7 mock endpoints hit
    let ids: Vec<&str> = report.providers.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "claude", "codex", "grok", "cursor", "agy", "copilot", "gemini"
        ]
    );
    assert_eq!(server.claude_hits.load(Ordering::SeqCst), 2);
    assert_eq!(server.codex_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server.grok_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server.agy_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server.cursor_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server.copilot_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server.gemini_hits.load(Ordering::SeqCst), 1);

    // Verify every bucket across all 7 providers populates `window_seconds` for burn-rate pacing!
    for prov in &report.providers {
        for acct in &prov.accounts {
            for bucket in &acct.buckets {
                assert!(
                    bucket.window_seconds.is_some(),
                    "bucket {}:{} missing window_seconds",
                    prov.id,
                    bucket.id
                );
            }
        }
    }

    // Verify 0600 permissions AND zero raw tokens/JWTs in state.json & collector-cache.json!
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let state_mode = fs::metadata(cache_store.state_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let cache_mode = fs::metadata(cache_store.collector_cache_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(state_mode, 0o600);
        assert_eq!(cache_mode, 0o600);
    }
    for path in [cache_store.state_path(), cache_store.collector_cache_path()] {
        let content = fs::read_to_string(&path).unwrap();
        for secret_needle in [
            "mock-oat01",
            "mock-ghu",
            "mock-ya29",
            "mock-1//",
            "csrf-e2e-token",
            "eyJhbGci",
        ] {
            assert!(
                !content.contains(secret_needle),
                "Persisted cache file {} leaked credential fragment {secret_needle}",
                path.display()
            );
        }
    }

    // Verify Grok deduplication (`~/.grok` + `~/.grok-swap` merged into 1 account!) & omitted-zero `0%`
    let grok = report.providers.iter().find(|p| p.id == "grok").unwrap();
    assert_eq!(grok.accounts.len(), 1);
    assert!(grok.accounts[0].is_system_default);
    assert_eq!(grok.primary_used_percent, Some(0));

    // Verify Antigravity 4 buckets + local RPC plan badge
    let agy = report.providers.iter().find(|p| p.id == "agy").unwrap();
    assert_eq!(agy.plan_badge.as_deref(), Some("Ultra"));
    let agy_labels: Vec<&str> = agy.accounts[0]
        .buckets
        .iter()
        .map(|b| b.label.as_str())
        .collect();
    assert_eq!(
        agy_labels,
        vec![
            "Gemini · Weekly",
            "Gemini · 5h",
            "Claude & GPT · Weekly",
            "Claude & GPT · 5h"
        ]
    );

    // Verify Cursor 3 buckets (`Cursor Models`, `Other Models`, `31d`) & `Ultra` plan badge
    let cursor = report.providers.iter().find(|p| p.id == "cursor").unwrap();
    assert_eq!(cursor.plan_badge.as_deref(), Some("Ultra"));
    assert_eq!(cursor.primary_used_percent, Some(12));
    let cursor_ids: Vec<&str> = cursor.accounts[0]
        .buckets
        .iter()
        .map(|b| b.id.as_str())
        .collect();
    assert_eq!(cursor_ids, vec!["cursor-models", "other-models", "31d"]);

    // Verify GitHub Copilot (`Pro`, 14% used, `42 / 300 premium requests used`, 31d window)
    let copilot = report
        .providers
        .iter()
        .find(|p| p.id == "copilot")
        .unwrap();
    assert_eq!(copilot.display_name, "GitHub Copilot");
    assert_eq!(copilot.icon_name, "provider-copilot");
    assert_eq!(copilot.plan_badge.as_deref(), Some("Pro"));
    assert_eq!(copilot.primary_used_percent, Some(14));
    assert_eq!(copilot.accounts[0].label, "octodev");
    assert_eq!(
        copilot.accounts[0].buckets[0].extra_info.as_deref(),
        Some("42 / 300 premium requests used")
    );
    assert_eq!(
        copilot.accounts[0].buckets[0].window_seconds,
        Some(2_678_400)
    );

    // Verify Gemini CLI (`Pro`, 22% Pro / 9% Flash, 24h window)
    let gemini = report.providers.iter().find(|p| p.id == "gemini").unwrap();
    assert_eq!(gemini.display_name, "Gemini CLI");
    assert_eq!(gemini.icon_name, "provider-gemini");
    assert_eq!(gemini.plan_badge.as_deref(), Some("Pro"));
    assert_eq!(gemini.primary_used_percent, Some(22));
    assert_eq!(gemini.accounts[0].label, "gem@cli.dev");
    assert_eq!(gemini.accounts[0].buckets.len(), 2);
    assert_eq!(gemini.accounts[0].buckets[0].label, "Gemini Pro · Daily");
    assert_eq!(gemini.accounts[0].buckets[0].window_seconds, Some(86_400));

    // Verify Claude starts on system default (`personal@home.io`, 24%), then `account select claude work`
    // runs `switch_command` and flips active rollup to `claude:work` (`89%`, `Max`, `OverallStatus::Warning`)!
    let claude = report.providers.iter().find(|p| p.id == "claude").unwrap();
    assert_eq!(claude.accounts.len(), 2);
    assert_eq!(claude.primary_used_percent, Some(24));

    let select_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "account",
        "select",
        "claude",
        "work",
    ]);
    let switched_out = execute_cli(
        &select_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: None,
            now: base_now,
            stdin_data: None,
        },
    )
    .expect("account select")
    .expect("account select stdout");
    assert_eq!(fs::read_to_string(&switch_marker).unwrap(), "work");
    let switched_report: StatusReport = serde_json::from_str(&switched_out).unwrap();
    let claude_after = switched_report
        .providers
        .iter()
        .find(|p| p.id == "claude")
        .unwrap();
    assert_eq!(claude_after.active_account_id.as_deref(), Some("claude:work"));
    assert_eq!(claude_after.primary_used_percent, Some(89));
    assert_eq!(claude_after.plan_badge.as_deref(), Some("Max"));
    assert_eq!(switched_report.status, OverallStatus::Warning);

    // Step 4: Verify HTTP 429 `Retry-After: 0` backoff preserves cached buckets and blocks `--force`!
    server.rate_limit_claude.store(true, Ordering::SeqCst);
    let after_debounce = base_now + chrono::Duration::seconds(45);
    let force_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "collect",
        "--force",
        "--provider",
        "claude",
    ]);
    let rl_out = execute_cli(
        &force_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: Some(fetch_ctx.clone()),
            now: after_debounce,
            stdin_data: None,
        },
    )
    .expect("collect 429")
    .expect("collect 429 stdout");
    let rl_report: StatusReport = serde_json::from_str(&rl_out).unwrap();
    let claude_rl = rl_report
        .providers
        .iter()
        .find(|p| p.id == "claude")
        .unwrap();
    let work_rl = claude_rl
        .accounts
        .iter()
        .find(|a| a.id == "claude:work")
        .unwrap();
    assert_eq!(work_rl.state, AccountState::RateLimited);
    assert!(work_rl.is_stale);
    assert_eq!(claude_rl.primary_used_percent, Some(89));
    let hits_after_429 = server.claude_hits.load(Ordering::SeqCst);

    // Calling `--force` again 60s later MUST NOT hit the HTTP server while 300s+ lock is active!
    let during_lock = after_debounce + chrono::Duration::seconds(60);
    execute_cli(
        &force_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: Some(fetch_ctx.clone()),
            now: during_lock,
            stdin_data: None,
        },
    )
    .expect("collect during 429 lock");
    assert_eq!(
        server.claude_hits.load(Ordering::SeqCst),
        hits_after_429,
        "429 backoff lock must block --force requests"
    );

    // Step 5: Verify `ingest-hook claude --account work` updates `wk`/`5h` with zero HTTP hits while preserving `fable`!
    let hook_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "ingest-hook",
        "claude",
        "--account",
        "work",
    ]);
    let hook_stdin = r#"{
        "rate_limits": {
            "five_hour": {"used_percentage": 52.0, "resets_at": 1794379200},
            "seven_day": {"used_percentage": 94.0, "resets_at": 1794492000}
        }
    }"#;
    let hook_stdout = execute_cli(
        &hook_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: None,
            now: during_lock,
            stdin_data: Some(hook_stdin),
        },
    )
    .expect("ingest-hook claude");
    assert!(
        hook_stdout.is_none(),
        "ingest-hook must stay silent on stdout by default for Claude Code statusLine TUI safety"
    );

    // Step 6: Verify `cached` returns updated `94%` (while keeping `fable`), then decays to `0%` after reset!
    let cached_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "cached",
    ]);
    let cached_now_out = execute_cli(
        &cached_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: None,
            now: during_lock,
            stdin_data: None,
        },
    )
    .expect("cached before reset")
    .expect("cached json");
    let cached_before: StatusReport = serde_json::from_str(&cached_now_out).unwrap();
    let claude_before = cached_before
        .providers
        .iter()
        .find(|p| p.id == "claude")
        .unwrap();
    assert_eq!(claude_before.primary_used_percent, Some(94));
    let work_before = claude_before
        .accounts
        .iter()
        .find(|a| a.id == "claude:work")
        .unwrap();
    assert!(work_before.buckets.iter().any(|b| b.id == "fable" && b.used_percent == 46));

    // Advance clock past all reset epochs -> passive time-decay resets buckets to 0% used / 100% remaining!
    let future_after_resets = Utc.with_ymd_and_hms(2026, 12, 5, 0, 0, 0).unwrap();
    let cached_decayed_out = execute_cli(
        &cached_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: None,
            now: future_after_resets,
            stdin_data: None,
        },
    )
    .expect("cached after reset")
    .expect("cached decayed json");
    let decayed_report: StatusReport = serde_json::from_str(&cached_decayed_out).unwrap();
    assert_eq!(decayed_report.summary.max_used_percent, 0);
    assert_eq!(decayed_report.status, OverallStatus::Ok);
    for prov in &decayed_report.providers {
        assert_eq!(prov.primary_used_percent, Some(0));
        assert_eq!(prov.primary_remaining_percent, Some(100));
    }

    // Step 7: Verify v2 `--text --color`, `--waybar`, and `--tmux` on both top-level and `cached`!
    let text_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "cached",
        "--text",
        "--color",
    ]);
    let text_out = execute_cli(
        &text_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: None,
            now: during_lock,
            stdin_data: None,
        },
    )
    .expect("cached --text --color")
    .expect("text output");
    assert!(text_out.contains("Claude [Max] (Work Claude)"));
    assert!(text_out.contains("Antigravity [Ultra]"));
    assert!(text_out.contains("Cursor [Ultra]"));
    assert!(text_out.contains("GitHub Copilot [Pro]"));
    assert!(text_out.contains("Gemini CLI [Pro]"));
    assert!(text_out.contains("\x1b[31m94%\x1b[0m"));

    // Top-level `gnome-agent-status --waybar` & `gnome-agent-status cached --waybar`
    for wb_args in [
        vec![
            "gnome-agent-status",
            "--config",
            config_path.to_str().unwrap(),
            "--waybar",
        ],
        vec![
            "gnome-agent-status",
            "--config",
            config_path.to_str().unwrap(),
            "cached",
            "--waybar",
        ],
    ] {
        let wb_cli = Cli::parse_from(wb_args);
        let wb_out = execute_cli(
            &wb_cli,
            &CliRuntime {
                cache_store: cache_store.clone(),
                fetch_ctx_override: Some(fetch_ctx.clone()),
                now: during_lock,
                stdin_data: None,
            },
        )
        .expect("waybar execution")
        .expect("waybar stdout");
        assert!(!wb_out.contains('\n'), "waybar JSON must be single-line");
        let wb_val: serde_json::Value = serde_json::from_str(&wb_out).expect("parse waybar JSON");
        assert_eq!(wb_val["class"], "critical");
        assert_eq!(wb_val["percentage"], 94);
        let wb_text = wb_val["text"].as_str().unwrap();
        assert!(wb_text.contains("CC 94%"));
        assert!(wb_text.contains("CP 14%"));
        assert!(wb_text.contains("GM 22%"));
        let wb_tip = wb_val["tooltip"].as_str().unwrap();
        assert!(wb_tip.contains("GitHub Copilot [Pro]"));
        assert!(wb_tip.contains("42 / 300 premium requests used"));
        assert!(wb_tip.contains("Gemini CLI [Pro]"));
    }

    // Top-level `gnome-agent-status --tmux` & `gnome-agent-status cached --tmux`
    for tmux_args in [
        vec![
            "gnome-agent-status",
            "--config",
            config_path.to_str().unwrap(),
            "--tmux",
        ],
        vec![
            "gnome-agent-status",
            "--config",
            config_path.to_str().unwrap(),
            "cached",
            "--tmux",
        ],
    ] {
        let tmux_cli = Cli::parse_from(tmux_args);
        let tmux_out = execute_cli(
            &tmux_cli,
            &CliRuntime {
                cache_store: cache_store.clone(),
                fetch_ctx_override: Some(fetch_ctx.clone()),
                now: during_lock,
                stdin_data: None,
            },
        )
        .expect("tmux execution")
        .expect("tmux stdout");
        assert!(!tmux_out.contains('\n'), "tmux output must be single-line");
        assert!(tmux_out.contains("#[fg=red,bold]CC 94%#[default]"));
        assert!(tmux_out.contains("#[fg=green]CP 14%#[default]"));
        assert!(tmux_out.contains("#[fg=green]GM 22%#[default]"));
    }

    let _ = fs::remove_dir_all(&sandbox);
}

#[test]
fn test_copilot_gh_404_noise_free_omission_and_gemini_passive_expiry_e2e() {
    let sandbox = unique_sandbox("noise-free-and-passive");
    let server = MockApiServer::start();
    server.copilot_unsubscribed.store(true, Ordering::SeqCst);

    let home = sandbox.join("home");
    let xdg_cfg = home.join(".config");
    let proc_root = sandbox.join("proc");
    fs::create_dir_all(xdg_cfg.join("gh")).unwrap();
    fs::create_dir_all(&proc_root).unwrap();

    // Seed `~/.config/gh/hosts.yml` (git-only user without Copilot) + `~/.gemini/antigravity` ONLY
    fs::write(
        xdg_cfg.join("gh").join("hosts.yml"),
        "github.com:\n    user: gitonly\n    oauth_token: mock-gho-gitonly\n",
    )
    .unwrap();
    fs::create_dir_all(home.join(".gemini").join("antigravity")).unwrap();

    let config_path = sandbox.join("config.toml");
    fs::write(
        &config_path,
        "auto_discover = true\n\n[providers.agy]\nenabled = false\n",
    )
    .unwrap();
    let cache_store = CacheStore::new(sandbox.join("cache"));
    let t0 = Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap();

    let fetch_ctx = FetchContext {
        http: HttpClient::new(Duration::from_secs(5)),
        timeout: Duration::from_secs(5),
        now: t0,
        home_dir: Some(home.clone()),
        xdg_config_dir: Some(xdg_cfg.clone()),
        proc_root: proc_root.clone(),
        endpoints: EndpointOverrides {
            copilot_user_url: Some(format!("{}/copilot_internal/user", server.base_url)),
            gemini_quota_url: Some(format!("{}/v1internal:retrieveUserQuota", server.base_url)),
            gemini_load_url: Some(format!("{}/v1internal:loadCodeAssist", server.base_url)),
            ..Default::default()
        },
        env_overrides: Some(std::collections::BTreeMap::new()),
    };

    let collect_cli = Cli::parse_from([
        "gnome-agent-status",
        "--config",
        config_path.to_str().unwrap(),
        "collect",
    ]);

    // 1. Both unsubscribed `gh` Copilot AND `~/.gemini/antigravity`-only Gemini must produce ZERO false error cards!
    let out_empty = execute_cli(
        &collect_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: Some(fetch_ctx.clone()),
            now: t0,
            stdin_data: None,
        },
    )
    .expect("collect empty")
    .expect("stdout");
    let rep_empty: StatusReport = serde_json::from_str(&out_empty).unwrap();
    assert!(
        rep_empty.providers.is_empty(),
        "expected zero false error cards for git-only gh user + antigravity dir, got {:?}",
        rep_empty.providers
    );
    assert_eq!(server.copilot_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server.gemini_hits.load(Ordering::SeqCst), 0);

    // 2. Now write valid `~/.gemini/oauth_creds.json` -> `collect` fetches active 22% quota
    let creds_path = home.join(".gemini").join("oauth_creds.json");
    let expiry_ms = (t0.timestamp() + 3600) * 1000;
    let valid_creds = format!(
        r#"{{"access_token":"mock-ya29-valid","refresh_token":"mock-1//ref","expiry_date":{expiry_ms}}}"#
    );
    fs::write(&creds_path, &valid_creds).unwrap();

    let out_active = execute_cli(
        &collect_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: Some(fetch_ctx.clone()),
            now: t0,
            stdin_data: None,
        },
    )
    .expect("collect active gemini")
    .expect("stdout");
    let rep_active: StatusReport = serde_json::from_str(&out_active).unwrap();
    assert_eq!(rep_active.providers.len(), 1);
    assert_eq!(rep_active.providers[0].id, "gemini");
    assert_eq!(rep_active.providers[0].primary_used_percent, Some(22));
    assert_eq!(
        rep_active.providers[0].accounts[0].state,
        AccountState::ActiveOk
    );
    assert_eq!(server.gemini_hits.load(Ordering::SeqCst), 1);

    // 3. Advance clock past `expiry_date` (`1794339600` = `t0 + 3600s`):
    //    `collect` must NOT rotate `refresh_token` or hit `retrieveUserQuota` again,
    //    leaving `oauth_creds.json` byte-for-byte untouched and degrading to `PassiveCached`!
    let t_expired = t0 + chrono::Duration::seconds(4000);
    let mut expired_ctx = fetch_ctx.clone();
    expired_ctx.now = t_expired;

    let out_passive = execute_cli(
        &collect_cli,
        &CliRuntime {
            cache_store: cache_store.clone(),
            fetch_ctx_override: Some(expired_ctx),
            now: t_expired,
            stdin_data: None,
        },
    )
    .expect("collect expired gemini")
    .expect("stdout");
    let rep_passive: StatusReport = serde_json::from_str(&out_passive).unwrap();
    assert_eq!(rep_passive.providers.len(), 1);
    let gem_acct = &rep_passive.providers[0].accounts[0];
    assert_eq!(gem_acct.state, AccountState::PassiveCached);
    assert_eq!(
        gem_acct.status_note.as_deref(),
        Some("Updates when Gemini CLI runs")
    );
    assert_eq!(rep_passive.providers[0].primary_used_percent, Some(22));
    assert_eq!(
        server.gemini_hits.load(Ordering::SeqCst),
        1,
        "expired oauth_creds.json must degrade passively with zero network calls"
    );
    assert_eq!(
        fs::read_to_string(&creds_path).unwrap(),
        valid_creds,
        "oauth_creds.json must remain strictly read-only"
    );

    let _ = fs::remove_dir_all(&sandbox);
}
