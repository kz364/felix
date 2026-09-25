//! Sign in with ChatGPT and call ChatGPT-backed models with the user's
//! subscription.
//!
//! This is the OAuth flow from OpenAI's open-source Codex CLI
//! (`codex-rs/login`): PKCE through a browser, a one-shot callback server on
//! localhost:1455, then Bearer calls to the Codex Responses endpoint. Handy
//! sends its own `originator`, as other open-source clients do. Tokens live in
//! a file only the user can read (not the Keychain, which prompts again after
//! every rebuild). Refresh tokens are single-use, so every refresh stores the
//! new one and refreshes never run concurrently.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ISSUER: &str = "https://auth.openai.com";
const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const ORIGINATOR: &str = "handy";
const SCOPES: &str = "openid profile email offline_access";
/// Ports registered for the Codex client's redirect URI, in order of preference.
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_SECS: i64 = 300;

/// Sign-in file, readable only by the user (like Codex's `~/.codex/auth.json`).
const AUTH_FILE: &str = "Library/Application Support/com.pais.handy/chatgpt-auth.json";

#[derive(Clone, Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    account_id: String,
    /// Unix seconds, from the access token's `exp` claim.
    expires_at: i64,
    #[serde(default)]
    email: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("account_id", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Serializes token refreshes (refresh tokens rotate and are single-use).
static TOKEN_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

// ---------------------------------------------------------------- storage

fn auth_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(AUTH_FILE))
}

fn load_tokens() -> Option<Tokens> {
    serde_json::from_slice(&std::fs::read(auth_path()?).ok()?).ok()
}

fn save_tokens(tokens: &Tokens) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = auth_path().ok_or("No home folder to save the ChatGPT sign-in in")?;
    let failed = |e: std::io::Error| format!("Couldn't save the ChatGPT sign-in: {e}");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    // Write a private temp file and rename it over, so a crash mid-write
    // never leaves a half-written sign-in (the refresh token is single-use).
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(failed)?;
    file.write_all(&serde_json::to_vec(tokens).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(failed)?;
    std::fs::rename(&tmp, &path).map_err(failed)
}

/// Forget the ChatGPT sign-in.
pub fn sign_out() {
    if let Some(path) = auth_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// The signed-in account's email, or `None` when not signed in.
pub fn signed_in_as() -> Option<String> {
    load_tokens().map(|t| t.email.unwrap_or_default())
}

// ---------------------------------------------------------------- JWT claims

fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

fn account_id(claims: &serde_json::Value) -> Option<String> {
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .map(str::to_string)
}

/// Build stored tokens from a token-endpoint response, keeping previous values
/// for fields a refresh didn't return.
fn tokens_from(response: TokenResponse, previous: Option<&Tokens>) -> Result<Tokens, String> {
    let access_token = response
        .access_token
        .ok_or("The sign-in response had no access token")?;
    let refresh_token = response
        .refresh_token
        .or_else(|| previous.map(|p| p.refresh_token.clone()))
        .ok_or("The sign-in response had no refresh token")?;
    let id_token = response
        .id_token
        .or_else(|| previous.map(|p| p.id_token.clone()))
        .unwrap_or_default();
    let access_claims = jwt_claims(&access_token).unwrap_or_default();
    let id_claims = jwt_claims(&id_token).unwrap_or_default();
    let account_id = account_id(&id_claims)
        .or_else(|| account_id(&access_claims))
        .or_else(|| previous.map(|p| p.account_id.clone()))
        .ok_or("This account has no ChatGPT workspace")?;
    let expires_at = access_claims["exp"]
        .as_i64()
        .or_else(|| {
            response
                .expires_in
                .map(|s| chrono::Utc::now().timestamp() + s)
        })
        .unwrap_or_else(|| chrono::Utc::now().timestamp() + 3600);
    let email = id_claims["email"]
        .as_str()
        .map(str::to_string)
        .or_else(|| previous.and_then(|p| p.email.clone()));
    Ok(Tokens {
        access_token,
        refresh_token,
        id_token,
        account_id,
        expires_at,
        email,
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: Option<serde_json::Value>,
    error_description: Option<String>,
}

async fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse, String> {
    let response = reqwest::Client::new()
        .post(format!("{ISSUER}/oauth/token"))
        .form(form)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach OpenAI to sign in: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_str::<OAuthError>(&body)
            .ok()
            .and_then(|e| {
                e.error_description
                    .or_else(|| e.error.map(|v| v.to_string()))
            })
            .unwrap_or_else(|| status.to_string());
        return Err(format!("ChatGPT sign-in failed: {detail}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("Unexpected sign-in response: {e}"))
}

// ---------------------------------------------------------------- login

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(buf)
}

fn authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    let mut url = url::Url::parse(&format!("{ISSUER}/oauth/authorize")).expect("valid issuer");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("scope", SCOPES)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", ORIGINATOR);
    url.to_string()
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Felix</title>\
<body style=\"font:16px -apple-system,sans-serif;display:grid;place-items:center;height:90vh\">\
<p>Signed in to ChatGPT. You can close this tab and go back to Felix.</p>";

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

/// Wait for the browser to come back to `/auth/callback` with the code.
async fn wait_for_code(listener: tokio::net::TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        else {
            continue;
        };
        let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
            respond(&mut stream, "400 Bad Request", "").await;
            continue;
        };
        if url.path() != "/auth/callback" {
            respond(&mut stream, "404 Not Found", "").await;
            continue;
        }
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        if let Some(error) = param("error") {
            let detail = param("error_description").unwrap_or(error);
            respond(
                &mut stream,
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
            )
            .await;
            return Err(format!("ChatGPT sign-in failed: {detail}"));
        }
        // ChatGPT may append a suffix to `state`; the prefix must match.
        if !param("state").is_some_and(|s| s.starts_with(state)) {
            respond(&mut stream, "400 Bad Request", "State mismatch.").await;
            continue;
        }
        let Some(code) = param("code") else {
            respond(&mut stream, "400 Bad Request", "Missing code.").await;
            continue;
        };
        respond(&mut stream, "200 OK", DONE_PAGE).await;
        return Ok(code);
    }
}

async fn bind_callback() -> Result<(tokio::net::TcpListener, u16), String> {
    for port in CALLBACK_PORTS {
        if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            return Ok((listener, port));
        }
    }
    Err("Ports 1455 and 1457 are in use (is another ChatGPT sign-in open?)".into())
}

/// Sign in through the browser. `open` is called with the URL to show.
/// Returns the account email.
pub async fn sign_in(open: impl FnOnce(&str) -> Result<(), String>) -> Result<String, String> {
    let verifier = random_b64(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(32);
    let (listener, port) = bind_callback().await?;
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    open(&authorize_url(&redirect_uri, &challenge, &state))?;

    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_code(listener, &state))
        .await
        .map_err(|_| "ChatGPT sign-in timed out".to_string())??;
    let response = token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("code_verifier", &verifier),
    ])
    .await?;
    let tokens = tokens_from(response, None)?;
    save_tokens(&tokens)?;
    Ok(tokens.email.unwrap_or_default())
}

// ---------------------------------------------------------------- tokens

async fn refresh(tokens: &Tokens) -> Result<Tokens, String> {
    let response = token_request(&[
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", &tokens.refresh_token),
    ])
    .await
    .map_err(|e| {
        if e.contains("refresh_token") {
            sign_out();
            "The ChatGPT sign-in expired. Sign in again in Settings.".to_string()
        } else {
            e
        }
    })?;
    let fresh = tokens_from(response, Some(tokens))?;
    save_tokens(&fresh)?;
    Ok(fresh)
}

/// Valid tokens, refreshed if they expire soon (or `force`).
async fn current_tokens(force: bool) -> Result<Tokens, String> {
    let _guard = TOKEN_LOCK.lock().await;
    let tokens = load_tokens().ok_or("Not signed in to ChatGPT")?;
    if force || tokens.expires_at - chrono::Utc::now().timestamp() < REFRESH_MARGIN_SECS {
        refresh(&tokens).await
    } else {
        Ok(tokens)
    }
}

// ---------------------------------------------------------------- inference

/// One request to a ChatGPT model.
pub struct Request<'a> {
    pub model: &'a str,
    /// Reasoning effort: "none", "low", "medium", "high".
    pub effort: &'a str,
    pub instructions: &'a str,
    pub input: &'a str,
    /// JSON Schema the reply must follow (structured output), if any.
    pub schema: Option<&'a serde_json::Value>,
}

static SESSION_ID: Lazy<String> = Lazy::new(|| uuid::Uuid::new_v4().to_string());

static HTTP: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("HTTP client")
});

fn request_body(request: &Request) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": request.model,
        "instructions": request.instructions,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": request.input}],
        }],
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {"effort": request.effort},
        "store": false,
        "stream": true,
        "prompt_cache_key": SESSION_ID.as_str(),
    });
    body["text"] = match request.schema {
        Some(schema) => serde_json::json!({
            "verbosity": "low",
            "format": {"type": "json_schema", "name": "reply", "strict": true, "schema": schema},
        }),
        None => serde_json::json!({"verbosity": "low"}),
    };
    body
}

/// Why a request failed, in words for the user.
fn http_error(status: reqwest::StatusCode, body: &str) -> String {
    let error = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
    let error = &error["error"];
    if error["type"] == "usage_limit_reached" {
        let resets = error["resets_at"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| format!(" until {}", t.with_timezone(&chrono::Local).format("%H:%M")))
            .unwrap_or_default();
        return format!("ChatGPT usage limit reached{resets}");
    }
    let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
    let message = error["message"]
        .as_str()
        .or_else(|| parsed["detail"].as_str())
        .unwrap_or_else(|| body.get(..body.len().min(300)).unwrap_or(""))
        .trim();
    if message.is_empty() {
        format!("ChatGPT request failed ({status})")
    } else {
        format!("ChatGPT request failed ({status}): {message}")
    }
}

/// Collect the reply text from the server-sent event stream.
/// Accumulates the reply from the Responses API's server-sent events.
#[derive(Default)]
struct Reply {
    pending: Vec<u8>,
    text: String,
}

impl Reply {
    /// Feed raw bytes; returns the outcome once the stream says it's finished.
    fn push(&mut self, bytes: &[u8]) -> Option<Result<String, String>> {
        self.pending.extend_from_slice(bytes);
        loop {
            let (end, sep) = find_event_end(&self.pending)?;
            let event: Vec<u8> = self.pending.drain(..end + sep).collect();
            if let Some(done) = self.event(&String::from_utf8_lossy(&event[..end])) {
                return Some(done);
            }
        }
    }

    fn event(&mut self, raw: &str) -> Option<Result<String, String>> {
        let data = raw
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let event = serde_json::from_str::<serde_json::Value>(&data).ok()?;
        match event["type"].as_str().unwrap_or("") {
            "response.output_text.delta" => {
                self.text.push_str(event["delta"].as_str().unwrap_or(""));
                None
            }
            "response.completed" | "response.done" => Some(Ok(std::mem::take(&mut self.text))),
            "response.failed" | "error" => {
                let error = if event["type"] == "error" {
                    &event
                } else {
                    &event["response"]["error"]
                };
                let message = error["message"].as_str().unwrap_or("unknown error");
                Some(Err(format!("ChatGPT request failed: {message}")))
            }
            "response.incomplete" => {
                let reason = event["response"]["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or("unknown reason");
                Some(Err(format!("ChatGPT stopped early ({reason})")))
            }
            _ => None,
        }
    }
}

/// Position and length of the first blank-line event separator.
fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    (0..buf.len()).find_map(|i| {
        if buf[i..].starts_with(b"\r\n\r\n") {
            Some((i, 4))
        } else if buf[i..].starts_with(b"\n\n") {
            Some((i, 2))
        } else {
            None
        }
    })
}

/// Collect the reply text from the server-sent event stream.
async fn read_stream(response: reqwest::Response) -> Result<String, String> {
    let mut stream = response.bytes_stream();
    let mut reply = Reply::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("ChatGPT stream broke: {e}"))?;
        if let Some(done) = reply.push(&chunk) {
            return done;
        }
    }
    if let Some(done) = reply.push(b"\n\n") {
        return done;
    }
    if reply.text.is_empty() {
        Err("ChatGPT closed the connection without a reply".into())
    } else {
        Ok(reply.text)
    }
}

async fn send(request: &Request<'_>, tokens: &Tokens) -> Result<reqwest::Response, reqwest::Error> {
    HTTP.post(ENDPOINT)
        .bearer_auth(&tokens.access_token)
        .header("ChatGPT-Account-ID", &tokens.account_id)
        .header("originator", ORIGINATOR)
        .header(
            "User-Agent",
            format!("{ORIGINATOR}/{}", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "text/event-stream")
        .header("session-id", SESSION_ID.as_str())
        .json(&request_body(request))
        .send()
        .await
}

/// Send one request and return the model's text reply.
pub async fn complete(request: Request<'_>) -> Result<String, String> {
    let mut tokens = current_tokens(false).await?;
    for attempt in 0..2 {
        let response = send(&request, &tokens)
            .await
            .map_err(|e| format!("Couldn't reach ChatGPT: {e}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            tokens = current_tokens(true).await?;
            continue;
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status, &body));
        }
        return read_stream(response).await.map(|t| t.trim().to_string());
    }
    Err("ChatGPT rejected the sign-in. Sign in again in Settings.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: serde_json::Value) -> String {
        format!(
            "e30.{}.sig",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    #[test]
    fn authorize_url_has_the_codex_parameters() {
        let url = authorize_url("http://localhost:1455/auth/callback", "chal", "st");
        let parsed = url::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.path(), "/oauth/authorize");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], "http://localhost:1455/auth/callback");
        assert_eq!(q["scope"], SCOPES);
        assert_eq!(q["originator"], "handy");
        assert_eq!(q["codex_cli_simplified_flow"], "true");
    }

    #[test]
    fn tokens_take_account_and_expiry_from_the_jwts() {
        let auth = serde_json::json!({"chatgpt_account_id": "acct_1"});
        let response = TokenResponse {
            access_token: Some(jwt(serde_json::json!({"exp": 2_000_000_000}))),
            refresh_token: Some("r1".into()),
            id_token: Some(jwt(
                serde_json::json!({"email": "a@b.c", "https://api.openai.com/auth": auth}),
            )),
            expires_in: None,
        };
        let tokens = tokens_from(response, None).unwrap();
        assert_eq!(tokens.account_id, "acct_1");
        assert_eq!(tokens.expires_at, 2_000_000_000);
        assert_eq!(tokens.email.as_deref(), Some("a@b.c"));

        // A refresh that returns only an access token keeps the rest.
        let refreshed = TokenResponse {
            access_token: Some(jwt(serde_json::json!({"exp": 2_000_003_600}))),
            refresh_token: None,
            id_token: None,
            expires_in: None,
        };
        let fresh = tokens_from(refreshed, Some(&tokens)).unwrap();
        assert_eq!(fresh.refresh_token, "r1");
        assert_eq!(fresh.account_id, "acct_1");
        assert_eq!(fresh.expires_at, 2_000_003_600);
    }

    #[test]
    fn usage_limit_errors_are_readable() {
        let body = r#"{"error":{"type":"usage_limit_reached","plan_type":"plus"}}"#;
        assert_eq!(
            http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, body),
            "ChatGPT usage limit reached"
        );
    }

    #[test]
    fn stream_collects_deltas_across_split_chunks() {
        let stream = "event: response.created\r\ndata: {\"type\":\"response.created\"}\r\n\r\n\
            data: {\"type\":\"response.output_text.delta\",\"delta\":\"Café \"}\n\n\
            data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n\
            data: {\"type\":\"response.completed\",\"response\":{}}\n\n";
        // Feed one byte at a time so a UTF-8 character straddles chunks.
        let mut reply = Reply::default();
        let mut outcome = None;
        for byte in stream.as_bytes() {
            if let Some(done) = reply.push(std::slice::from_ref(byte)) {
                outcome = Some(done);
                break;
            }
        }
        assert_eq!(outcome, Some(Ok("Café ok".to_string())));

        let mut reply = Reply::default();
        let failed =
            r#"data: {"type":"response.failed","response":{"error":{"message":"bad model"}}}"#;
        assert_eq!(
            reply.push(format!("{failed}\n\n").as_bytes()),
            Some(Err("ChatGPT request failed: bad model".to_string()))
        );
    }
}
