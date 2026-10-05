//! Sign in with ChatGPT and call ChatGPT models with the user's plan.
//!
//! OpenAI's documented flow for open-source apps ("ChatGPT plan usage",
//! developers.openai.com/siwc/token-sharing-open-source): the first sign-in
//! registers Felix as the user's own app (`client_id=dynamic_agent_client`,
//! named "Felix", tied to this Mac's stable host ID) and asks to use their
//! plan; later sign-ins reuse the issued client ID. PKCE through the browser
//! to a one-shot callback on 127.0.0.1, the ID token checked against
//! OpenAI's published keys, then Bearer calls to the public Responses API
//! (`store: false`, `stream: true`). The plan can't transcribe audio: the
//! flow only covers `POST /v1/responses`.
//!
//! The older sign-in borrowed the Codex CLI's client and called ChatGPT's
//! `backend-api`. It's deprecated: an existing one keeps working until the
//! user signs in again, which deletes it; nothing starts it any more.
//!
//! Tokens live in files only the user can read (not the Keychain, which
//! prompts again after every rebuild). Refresh tokens rotate and are
//! single-use, so every refresh stores the new one and refreshes never run
//! concurrently.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
/// The audience of plan-usage tokens, sent as `resource`.
const RESOURCE: &str = "https://api.openai.com/v1";
const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
/// First-time registration entry point; never saved or used for tokens.
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
/// The app's name on the registration (the user can edit it).
const AGENT_NAME: &str = "Felix";
/// Without this granted, the sign-in can't use the plan.
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// Where users see and limit what Felix uses of their plan.
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// Callback port tried first; any free one works (only the port may vary).
const CALLBACK_PORT: u16 = 1455;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_SECS: i64 = 300;
/// Refresh errors that mean the refresh token is no use: sign in again.
const DEAD_REFRESH: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// The sign-in: issued client ID, identity and tokens, readable only by the
/// user.
const SIGN_IN_FILE: &str = "Library/Application Support/com.pais.handy/chatgpt-sign-in.json";
/// This Mac's host ID, chosen once and kept across sign-ins.
const HOST_ID_FILE: &str = "Library/Application Support/com.pais.handy/chatgpt-host-id";

// The deprecated sign-in (Codex CLI's client, ChatGPT's backend-api).
const LEGACY_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const LEGACY_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const LEGACY_ORIGINATOR: &str = "handy";
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

fn home_path(file: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(file))
}

fn auth_path() -> Option<std::path::PathBuf> {
    home_path(AUTH_FILE)
}

fn load_tokens() -> Option<Tokens> {
    serde_json::from_slice(&std::fs::read(auth_path()?).ok()?).ok()
}

/// Write a file only the user can read, atomically: a private temp file
/// renamed over, so a crash mid-write never leaves a half-written sign-in
/// (refresh tokens are single-use).
fn save_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let failed = |e: std::io::Error| format!("Couldn't save the ChatGPT sign-in: {e}");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(failed)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(failed)?;
    std::fs::rename(&tmp, path).map_err(failed)
}

fn save_tokens(tokens: &Tokens) -> Result<(), String> {
    let path = auth_path().ok_or("No home folder to save the ChatGPT sign-in in")?;
    save_private(
        &path,
        &serde_json::to_vec(tokens).map_err(|e| e.to_string())?,
    )
}

/// Felix's registration with the user's ChatGPT account: kept after signing
/// out (without tokens), so signing in again reuses the issued client.
#[derive(Clone, Default, Serialize, Deserialize)]
struct SignIn {
    issuer: String,
    /// The validated ID token's `sub`.
    subject: String,
    email: Option<String>,
    /// Issued at the first sign-in (`oaiapp_…`).
    client_id: String,
    ext_agent_host_id: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    /// Unix seconds.
    #[serde(default)]
    expires_at: i64,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    saved_at: String,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("client_id", &self.client_id)
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl SignIn {
    fn signed_in(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    fn uses_plan(&self) -> bool {
        self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }
}

fn load_sign_in() -> Option<SignIn> {
    serde_json::from_slice(&std::fs::read(home_path(SIGN_IN_FILE)?).ok()?).ok()
}

fn save_sign_in(sign_in: &SignIn) -> Result<(), String> {
    let path = home_path(SIGN_IN_FILE).ok_or("No home folder to save the ChatGPT sign-in in")?;
    save_private(
        &path,
        &serde_json::to_vec(sign_in).map_err(|e| e.to_string())?,
    )
}

/// This Mac's `ext_agent_host_id`: a `urn:uuid:` made once and kept, so the
/// plan's usage is told apart per Mac (an identifier, not a credential).
fn host_id() -> Result<String, String> {
    let path = home_path(HOST_ID_FILE).ok_or("No home folder")?;
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if id.starts_with("urn:uuid:") {
            return Ok(id.to_string());
        }
    }
    let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    save_private(&path, id.as_bytes())?;
    Ok(id)
}

/// The deprecated sign-in is all there is (sign in again to move on).
pub fn uses_legacy_sign_in() -> bool {
    !load_sign_in().is_some_and(|s| s.signed_in()) && load_tokens().is_some()
}

fn forget_legacy() {
    if let Some(path) = auth_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Sign out: end the session at OpenAI, then forget the tokens (keeping the
/// registration and host ID for next time). `Err` says the session couldn't
/// be ended remotely; the tokens are forgotten either way.
pub async fn sign_out() -> Result<(), String> {
    forget_legacy();
    let Some(mut sign_in) = load_sign_in().filter(SignIn::signed_in) else {
        return Ok(());
    };
    let revoked = revoke(&sign_in).await;
    sign_in.id_token.clear();
    sign_in.access_token.clear();
    sign_in.refresh_token.clear();
    sign_in.expires_at = 0;
    save_sign_in(&sign_in)?;
    revoked.map_err(|e| {
        format!(
            "Signed out on this Mac, but OpenAI didn't confirm ({e}). You can disconnect Felix in ChatGPT settings."
        )
    })
}

/// Revoke the renewable session, retrying network and server errors.
async fn revoke(sign_in: &SignIn) -> Result<(), String> {
    let discovery = discovery().await?;
    let endpoint = discovery["revocation_endpoint"]
        .as_str()
        .ok_or("no revocation endpoint")?
        .to_string();
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
        }
        match reqwest::Client::new()
            .post(&endpoint)
            .form(&[
                ("token", sign_in.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", sign_in.client_id.as_str()),
            ])
            .timeout(Duration::from_secs(20))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) if r.status().is_server_error() => last = r.status().to_string(),
            Ok(r) => return Err(r.status().to_string()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// The signed-in account's email, or `None` when not signed in.
pub fn signed_in_as() -> Option<String> {
    if let Some(sign_in) = load_sign_in().filter(SignIn::signed_in) {
        return Some(sign_in.email.unwrap_or_default());
    }
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
    /// Granted scopes, space-separated.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: Option<serde_json::Value>,
    error_description: Option<String>,
}

/// A token-endpoint failure: the OAuth error code and words for the user.
struct TokenError {
    code: String,
    message: String,
}

async fn token_request(url: &str, form: &[(&str, &str)]) -> Result<TokenResponse, TokenError> {
    let response = reqwest::Client::new()
        .post(url)
        .form(form)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| TokenError {
            code: String::new(),
            message: format!("Couldn't reach OpenAI to sign in: {e}"),
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let error = serde_json::from_str::<OAuthError>(&body).ok();
        let code = error
            .as_ref()
            .and_then(|e| e.error.as_ref())
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        let detail = error
            .and_then(|e| e.error_description)
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| {
                if code.is_empty() {
                    status.to_string()
                } else {
                    code.clone()
                }
            });
        return Err(TokenError {
            code,
            message: format!("ChatGPT sign-in failed: {detail}"),
        });
    }
    serde_json::from_str(&body).map_err(|e| TokenError {
        code: String::new(),
        message: format!("Unexpected sign-in response: {e}"),
    })
}

/// OpenAI's OpenID configuration (JWKS and revocation endpoints).
async fn discovery() -> Result<serde_json::Value, String> {
    get_json(DISCOVERY_URL).await
}

async fn get_json(url: &str) -> Result<serde_json::Value, String> {
    reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't reach OpenAI: {e}"))?
        .json()
        .await
        .map_err(|e| format!("OpenAI sent an unreadable reply: {e}"))
}

// ---------------------------------------------------------------- ID token

/// Check an ID token: RS256 signature by a key in `jwks`, issuer, audience
/// (the issued client), expiry and the attempt's nonce. Returns its claims.
fn check_id_token(
    token: &str,
    jwks: &serde_json::Value,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> Result<serde_json::Value, String> {
    let bad = |why: &str| format!("ChatGPT's sign-in couldn't be verified ({why})");
    let mut parts = token.split('.');
    let (Some(head), Some(body), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("not a JWT"));
    };
    let decode = |part: &str| URL_SAFE_NO_PAD.decode(part.trim_end_matches('='));
    let header: serde_json::Value = decode(head)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| bad("header"))?;
    if header["alg"] != "RS256" {
        return Err(bad("algorithm"));
    }
    let key = jwks["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|k| k["kty"] == "RSA" && (header["kid"].is_null() || k["kid"] == header["kid"]))
        .ok_or_else(|| bad("unknown key"))?;
    let component = |name: &str| {
        key[name]
            .as_str()
            .and_then(|v| decode(v).ok())
            .ok_or_else(|| bad("key"))
    };
    let (n, e) = (component("n")?, component("e")?);
    let signature = decode(sig).map_err(|_| bad("signature"))?;
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{head}.{body}").as_bytes(),
            &signature,
        )
        .map_err(|_| bad("signature"))?;
    let claims: serde_json::Value = decode(body)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| bad("claims"))?;
    if claims["iss"] != ISSUER {
        return Err(bad("issuer"));
    }
    let audience_ok = match &claims["aud"] {
        serde_json::Value::String(a) => a == client_id,
        serde_json::Value::Array(a) => a.iter().any(|v| v == client_id),
        _ => false,
    };
    if !audience_ok {
        return Err(bad("audience"));
    }
    if claims["exp"].as_i64().is_none_or(|exp| exp < now - 60) {
        return Err(bad("expired"));
    }
    if claims["nonce"] != nonce {
        return Err(bad("nonce"));
    }
    if claims["sub"].as_str().is_none_or(str::is_empty) {
        return Err(bad("subject"));
    }
    Ok(claims)
}

async fn verify_id_token(
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<serde_json::Value, String> {
    let jwks_uri = discovery().await?["jwks_uri"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{ISSUER}/.well-known/jwks.json"));
    let jwks = get_json(&jwks_uri).await?;
    check_id_token(
        token,
        &jwks,
        client_id,
        nonce,
        chrono::Utc::now().timestamp(),
    )
}

// ---------------------------------------------------------------- login

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(buf)
}

/// What an authorization attempt sends.
struct Attempt<'a> {
    redirect_uri: &'a str,
    challenge: &'a str,
    state: &'a str,
    nonce: &'a str,
    host_id: &'a str,
}

/// The authorize URL: first-time registration (`dynamic_agent_client`, with
/// Felix's name) or signing in again with the saved registration.
fn authorize_url(attempt: &Attempt, saved: Option<&SignIn>) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("valid authorize URL");
    {
        let mut q = url.query_pairs_mut();
        match saved {
            Some(saved) => {
                q.append_pair("client_id", &saved.client_id);
                if !saved.id_token.is_empty() {
                    q.append_pair("id_token_hint", &saved.id_token);
                }
                if let Some(email) = saved.email.as_deref().filter(|e| !e.is_empty()) {
                    q.append_pair("login_hint", email);
                }
            }
            None => {
                q.append_pair("client_id", DYNAMIC_CLIENT);
                q.append_pair("agent_name_hint", AGENT_NAME);
            }
        }
        q.append_pair("ext_agent_host_id", attempt.host_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", attempt.redirect_uri)
            .append_pair("scope", SCOPES)
            .append_pair("resource", RESOURCE)
            .append_pair("state", attempt.state)
            .append_pair("nonce", attempt.nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", attempt.challenge);
    }
    url.to_string()
}

/// The client to exchange the code with: the saved one (a callback naming
/// another is refused), or the one a first registration issued.
fn client_for(saved: Option<&str>, returned: Option<&str>) -> Result<String, String> {
    match (saved, returned.filter(|c| !c.is_empty())) {
        (Some(saved), Some(returned)) if saved != returned => {
            Err("ChatGPT answered for a different Felix registration; try signing in again".into())
        }
        (Some(saved), _) => Ok(saved.to_string()),
        (None, Some(returned)) if returned != DYNAMIC_CLIENT => Ok(returned.to_string()),
        (None, _) => Err("ChatGPT didn't finish registering Felix; try signing in again".into()),
    }
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

/// What the browser brought back to `/auth/callback`.
struct Callback {
    code: String,
    client_id: Option<String>,
}

async fn wait_for_code(listener: tokio::net::TcpListener, state: &str) -> Result<Callback, String> {
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
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
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
        if param("state").as_deref() != Some(state) {
            respond(&mut stream, "400 Bad Request", "State mismatch.").await;
            continue;
        }
        if let Some(error) = param("error") {
            respond(
                &mut stream,
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
            )
            .await;
            return Err(if error == "access_denied" {
                "Sign-in was declined, so Felix can't use your ChatGPT plan".to_string()
            } else {
                format!(
                    "ChatGPT sign-in failed: {}",
                    param("error_description").unwrap_or(error)
                )
            });
        }
        let Some(code) = param("code") else {
            respond(&mut stream, "400 Bad Request", "Missing code.").await;
            continue;
        };
        respond(&mut stream, "200 OK", DONE_PAGE).await;
        return Ok(Callback {
            code,
            client_id: param("client_id"),
        });
    }
}

/// Port 1455 if it's free, any other otherwise (only the port may vary).
async fn bind_callback() -> Result<(tokio::net::TcpListener, u16), String> {
    for port in [CALLBACK_PORT, 0] {
        if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            return Ok((listener, port));
        }
    }
    Err("Couldn't open a port for the ChatGPT sign-in".into())
}

/// Sign in through the browser, registering Felix the first time. `open` is
/// called with the URL to show. Returns the account email. Replaces the
/// deprecated sign-in once it works.
pub async fn sign_in(open: impl FnOnce(&str) -> Result<(), String>) -> Result<String, String> {
    let host = host_id()?;
    let saved = load_sign_in().filter(|s| !s.client_id.is_empty());
    let verifier = random_b64(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let (state, nonce) = (random_b64(32), random_b64(32));
    let (listener, port) = bind_callback().await?;
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let attempt = Attempt {
        redirect_uri: &redirect_uri,
        challenge: &challenge,
        state: &state,
        nonce: &nonce,
        host_id: &host,
    };
    open(&authorize_url(&attempt, saved.as_ref()))?;

    let callback = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_code(listener, &state))
        .await
        .map_err(|_| "ChatGPT sign-in timed out".to_string())??;
    let client_id = client_for(
        saved.as_ref().map(|s| s.client_id.as_str()),
        callback.client_id.as_deref(),
    )?;
    let response = token_request(
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &callback.code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect_uri),
            ("resource", RESOURCE),
        ],
    )
    .await
    .map_err(|e| e.message)?;
    let id_token = response
        .id_token
        .clone()
        .ok_or("The sign-in response had no ID token")?;
    let claims = verify_id_token(&id_token, &client_id, &nonce).await?;
    let subject = claims["sub"].as_str().unwrap_or_default().to_string();
    if let Some(saved) = saved.as_ref().filter(|s| !s.subject.is_empty()) {
        if saved.subject != subject {
            return Err(format!(
                "That's a different ChatGPT account from the one Felix uses ({}). Sign in with that one.",
                saved.email.as_deref().unwrap_or("signed out")
            ));
        }
    }
    let sign_in = sign_in_from(response, &claims, client_id, host)?;
    save_sign_in(&sign_in)?;
    forget_legacy();
    if !sign_in.uses_plan() {
        return Err(
            "Signed in, but Felix wasn't allowed to use your ChatGPT plan. Sign in again and allow it."
                .into(),
        );
    }
    Ok(sign_in.email.unwrap_or_default())
}

/// The saved sign-in from a code exchange and its checked ID-token claims.
fn sign_in_from(
    response: TokenResponse,
    claims: &serde_json::Value,
    client_id: String,
    host: String,
) -> Result<SignIn, String> {
    let now = chrono::Utc::now();
    Ok(SignIn {
        issuer: ISSUER.to_string(),
        subject: claims["sub"].as_str().unwrap_or_default().to_string(),
        email: claims["email"].as_str().map(str::to_string),
        client_id,
        ext_agent_host_id: host,
        id_token: response.id_token.unwrap_or_default(),
        access_token: response
            .access_token
            .ok_or("The sign-in response had no access token")?,
        refresh_token: response
            .refresh_token
            .ok_or("The sign-in response had no refresh token")?,
        expires_at: now.timestamp() + response.expires_in.unwrap_or(3600),
        scopes: response
            .scope
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        saved_at: now.to_rfc3339(),
    })
}

// ---------------------------------------------------------------- tokens

/// Renew the plan-usage tokens; a dead refresh token signs out (keeping the
/// registration) and asks for a new sign-in.
async fn refresh(sign_in: &SignIn) -> Result<SignIn, String> {
    let response = token_request(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &sign_in.client_id),
            ("refresh_token", &sign_in.refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await;
    let response = match response {
        Ok(r) => r,
        Err(e) if DEAD_REFRESH.contains(&e.code.as_str()) => {
            let mut signed_out = sign_in.clone();
            signed_out.access_token.clear();
            signed_out.refresh_token.clear();
            signed_out.expires_at = 0;
            save_sign_in(&signed_out)?;
            return Err("The ChatGPT sign-in expired. Sign in again in Settings.".into());
        }
        Err(e) => return Err(e.message),
    };
    let fresh = refreshed(sign_in, response)?;
    save_sign_in(&fresh)?;
    Ok(fresh)
}

/// A sign-in with a refresh's replacement tokens (taken together).
fn refreshed(sign_in: &SignIn, response: TokenResponse) -> Result<SignIn, String> {
    let now = chrono::Utc::now();
    let mut fresh = sign_in.clone();
    fresh.access_token = response
        .access_token
        .ok_or("The refresh response had no access token")?;
    if let Some(refresh_token) = response.refresh_token {
        fresh.refresh_token = refresh_token;
    }
    if let Some(id_token) = response.id_token {
        fresh.id_token = id_token;
    }
    if let Some(scope) = response.scope {
        fresh.scopes = scope.split_whitespace().map(str::to_string).collect();
    }
    fresh.expires_at = now.timestamp() + response.expires_in.unwrap_or(3600);
    fresh.saved_at = now.to_rfc3339();
    Ok(fresh)
}

/// The deprecated sign-in's refresh.
async fn refresh_legacy(tokens: &Tokens) -> Result<Tokens, String> {
    let response = token_request(
        &format!("{ISSUER}/oauth/token"),
        &[
            ("grant_type", "refresh_token"),
            ("client_id", LEGACY_CLIENT_ID),
            ("refresh_token", &tokens.refresh_token),
        ],
    )
    .await
    .map_err(|e| {
        if e.message.contains("refresh_token") || DEAD_REFRESH.contains(&e.code.as_str()) {
            forget_legacy();
            "The ChatGPT sign-in expired. Sign in again in Settings.".to_string()
        } else {
            e.message
        }
    })?;
    let fresh = tokens_from(response, Some(tokens))?;
    save_tokens(&fresh)?;
    Ok(fresh)
}

/// Which sign-in requests go out with.
enum Auth {
    Plan(SignIn),
    /// Deprecated.
    Legacy(Tokens),
}

/// Valid credentials, refreshed if they expire soon (or `force`).
async fn current_auth(force: bool) -> Result<Auth, String> {
    let _guard = TOKEN_LOCK.lock().await;
    let soon = |expires_at: i64| expires_at - chrono::Utc::now().timestamp() < REFRESH_MARGIN_SECS;
    if let Some(sign_in) = load_sign_in().filter(SignIn::signed_in) {
        if !sign_in.uses_plan() {
            return Err(
                "Felix isn't allowed to use your ChatGPT plan. Sign in again in Settings and allow it."
                    .into(),
            );
        }
        return Ok(Auth::Plan(if force || soon(sign_in.expires_at) {
            refresh(&sign_in).await?
        } else {
            sign_in
        }));
    }
    let tokens = load_tokens().ok_or("Not signed in to ChatGPT")?;
    Ok(Auth::Legacy(if force || soon(tokens.expires_at) {
        refresh_legacy(&tokens).await?
    } else {
        tokens
    }))
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
    let plan = match error["code"].as_str().unwrap_or("") {
        "subscription_sharing_usage_limit_exceeded" => Some(format!(
            "ChatGPT usage limit reached for Felix. Manage it at {USAGE_URL}"
        )),
        "subscription_sharing_user_not_eligible" => {
            Some("This ChatGPT account or workspace can't share its plan with Felix".into())
        }
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            Some("ChatGPT couldn't check your plan's usage just now; try again shortly".into())
        }
        _ => None,
    };
    if let Some(plan) = plan {
        return plan;
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
                if error["code"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("subscription_sharing_"))
                {
                    return Some(Err(http_error(
                        reqwest::StatusCode::OK,
                        &serde_json::json!({ "error": error }).to_string(),
                    )));
                }
                let message = error["message"]
                    .as_str()
                    .filter(|m| !m.is_empty())
                    .or_else(|| error["code"].as_str())
                    .unwrap_or("unknown error");
                if message == "unknown error" {
                    log::warn!("ChatGPT failed with no message: {}", error);
                }
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

async fn send(request: &Request<'_>, auth: &Auth) -> Result<reqwest::Response, reqwest::Error> {
    let post = match auth {
        Auth::Plan(sign_in) => HTTP.post(RESPONSES_URL).bearer_auth(&sign_in.access_token),
        Auth::Legacy(tokens) => HTTP
            .post(LEGACY_ENDPOINT)
            .bearer_auth(&tokens.access_token)
            .header("ChatGPT-Account-ID", &tokens.account_id)
            .header("originator", LEGACY_ORIGINATOR)
            .header("session-id", SESSION_ID.as_str()),
    };
    post.header(
        "User-Agent",
        format!("{AGENT_NAME}/{}", env!("CARGO_PKG_VERSION")),
    )
    .header("Accept", "text/event-stream")
    .json(&request_body(request))
    .send()
    .await
}

/// Send one request and return the model's text reply.
pub async fn complete(request: Request<'_>) -> Result<String, String> {
    let mut auth = current_auth(false).await?;
    if matches!(auth, Auth::Legacy(_)) {
        log::warn!("ChatGPT: using the deprecated sign-in; sign in again to move to the new one");
    }
    for attempt in 0..2 {
        let response = send(&request, &auth)
            .await
            .map_err(|e| format!("Couldn't reach ChatGPT: {e}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            auth = current_auth(true).await?;
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
    fn tokens_take_account_and_expiry_from_the_jwts() {
        let auth = serde_json::json!({"chatgpt_account_id": "acct_1"});
        let response = TokenResponse {
            access_token: Some(jwt(serde_json::json!({"exp": 2_000_000_000}))),
            refresh_token: Some("r1".into()),
            id_token: Some(jwt(
                serde_json::json!({"email": "a@b.c", "https://api.openai.com/auth": auth}),
            )),
            expires_in: None,
            scope: None,
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
            scope: None,
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

    /// A throwaway RSA key's public half and an ID token it signed, for
    /// fictional Sam Rivera (aud oaiapp_test, nonce n0nce, exp 2100).
    const TEST_N: &str = "sw4DmaMopuoHUyGrJnAQEElxRRJK0z3Br3ZeqUrXpeqy_tztTeSEVzT_ogAxezkVLiCTwfap5wl9kV7vYuP_X_YbIRJnC5toyUSXIazFKeGUeD7cuyrYDwpGHjLBoFiH_AtPlzy5YhNJeQgTzo7akr0yv7v54fO1kVvzHz8yNJFGebP7Ef040bn6zciBV7_iRnIF_4JoqV8uwluV1OUr_-Iyzh2Wz80uCXx-iRYnE6FYxNG26q7iI-dcEDAM2kJ37ieqnGnAPbkS7Y-EkBwsHHJNBJe5BZ-gYhU74i9jTK8ndIS7R9YGv8IbWz6lBPLdvRfBqK0D-awPrqaynu3d5Q";
    const TEST_TOKEN: &str = "eyJhbGciOiJSUzI1NiIsImtpZCI6InRlc3Qta2V5IiwidHlwIjoiSldUIn0.eyJpc3MiOiJodHRwczovL2F1dGgub3BlbmFpLmNvbSIsImF1ZCI6WyJvYWlhcHBfdGVzdCJdLCJzdWIiOiJ1c2VyLXNhbSIsImVtYWlsIjoic2FtLnJpdmVyYUBleGFtcGxlLmNvbSIsIm5vbmNlIjoibjBuY2UiLCJleHAiOjQxMDI0NDQ4MDAsImlhdCI6MTc5MDAwMDAwMH0.DayNRNuJvl_Bv2R2Si2sQRunxHIeW-54PIR0hvOB7Udu38e80oHtZlmUI12eMSZwJqqboHCkEIMc47uX50bnr25TeJcVovKbiy-EaYrMTw93Op-O2nsgadJJJdvYe08WidKitciRqmAps7arJTVWaFVaxeUUU7vo0ZY0olDHB2QP2tc1YcV7CNGKDSQ1Bv9roAdpNJMB6mimuFjgTHhkgHq_9siAShVWhBvJ1quXX8zwQ7q_ITmfl51p9LvGXIURndpZCaY90RneuTSOdVkKk7vziEVY1ltk93ecHt87Ea3VipM_Vcib3bBG5kMUvEqBBAd_-vYba2JWJNt1dxf27Q";

    fn test_jwks() -> serde_json::Value {
        serde_json::json!({"keys": [
            {"kty": "RSA", "kid": "other", "n": "AQAB", "e": "AQAB"},
            {"kty": "RSA", "kid": "test-key", "n": TEST_N, "e": "AQAB"},
        ]})
    }

    #[test]
    fn id_tokens_are_checked_against_the_published_key() {
        let now = 1_790_000_000;
        let claims = check_id_token(TEST_TOKEN, &test_jwks(), "oaiapp_test", "n0nce", now).unwrap();
        assert_eq!(claims["sub"], "user-sam");
        assert_eq!(claims["email"], "sam.rivera@example.com");

        let e = |client: &str, nonce: &str, now: i64| {
            check_id_token(TEST_TOKEN, &test_jwks(), client, nonce, now).unwrap_err()
        };
        assert!(e("oaiapp_other", "n0nce", now).contains("audience"));
        assert!(e("oaiapp_test", "other", now).contains("nonce"));
        assert!(e("oaiapp_test", "n0nce", 4_200_000_000).contains("expired"));

        // A changed claim breaks the signature.
        let mut parts: Vec<String> = TEST_TOKEN.split('.').map(str::to_string).collect();
        let mut forged: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[1]).unwrap()).unwrap();
        forged["sub"] = "someone-else".into();
        parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        let err = check_id_token(&parts.join("."), &test_jwks(), "oaiapp_test", "n0nce", now)
            .unwrap_err();
        assert!(err.contains("signature"), "{err}");
    }

    fn attempt_url(saved: Option<&SignIn>) -> std::collections::HashMap<String, String> {
        let attempt = Attempt {
            redirect_uri: "http://127.0.0.1:1455/auth/callback",
            challenge: "chal",
            state: "st",
            nonce: "nn",
            host_id: "urn:uuid:00000000-0000-4000-8000-000000000000",
        };
        let url = url::Url::parse(&authorize_url(&attempt, saved)).unwrap();
        assert_eq!(url.as_str().split('?').next(), Some(AUTHORIZE_URL));
        url.query_pairs().into_owned().collect()
    }

    #[test]
    fn the_first_sign_in_registers_felix_and_later_ones_reuse_it() {
        let q = attempt_url(None);
        assert_eq!(q["client_id"], "dynamic_agent_client");
        assert_eq!(q["agent_name_hint"], "Felix");
        assert_eq!(
            q["ext_agent_host_id"],
            "urn:uuid:00000000-0000-4000-8000-000000000000"
        );
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:1455/auth/callback");
        assert_eq!(q["resource"], "https://api.openai.com/v1");
        assert!(q["scope"]
            .split(' ')
            .any(|s| s == "chatgpt.tokens.use.direct"));
        assert!(q["scope"].split(' ').any(|s| s == "resource.invoke"));
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["nonce"], "nn");
        assert!(!q.contains_key("id_token_hint"));

        let saved = SignIn {
            client_id: "oaiapp_1".into(),
            email: Some("sam.rivera@example.com".into()),
            id_token: "old.id.token".into(),
            ..SignIn::default()
        };
        let q = attempt_url(Some(&saved));
        assert_eq!(q["client_id"], "oaiapp_1");
        assert!(!q.contains_key("agent_name_hint"));
        assert_eq!(q["id_token_hint"], "old.id.token");
        assert_eq!(q["login_hint"], "sam.rivera@example.com");
    }

    #[test]
    fn the_callback_client_must_match_the_registration() {
        assert_eq!(client_for(None, Some("oaiapp_new")).unwrap(), "oaiapp_new");
        assert!(client_for(None, None).is_err());
        assert!(client_for(None, Some("dynamic_agent_client")).is_err());
        assert_eq!(client_for(Some("oaiapp_1"), None).unwrap(), "oaiapp_1");
        assert_eq!(
            client_for(Some("oaiapp_1"), Some("oaiapp_1")).unwrap(),
            "oaiapp_1"
        );
        assert!(client_for(Some("oaiapp_1"), Some("oaiapp_2")).is_err());
    }

    #[test]
    fn a_refresh_replaces_the_tokens_together_and_plan_use_follows_the_scopes() {
        let sign_in = SignIn {
            client_id: "oaiapp_1".into(),
            access_token: "a1".into(),
            refresh_token: "r1".into(),
            id_token: "i1".into(),
            scopes: vec!["openid".into(), PLAN_SCOPE.into()],
            ..SignIn::default()
        };
        assert!(sign_in.signed_in() && sign_in.uses_plan());
        let fresh = refreshed(
            &sign_in,
            TokenResponse {
                access_token: Some("a2".into()),
                refresh_token: Some("r2".into()),
                id_token: None,
                expires_in: Some(3600),
                scope: Some("openid email".into()),
            },
        )
        .unwrap();
        assert_eq!(
            (fresh.access_token.as_str(), fresh.refresh_token.as_str()),
            ("a2", "r2")
        );
        assert_eq!(fresh.id_token, "i1", "kept for the next sign-in's hint");
        assert!(!fresh.uses_plan());
        assert_eq!(fresh.client_id, "oaiapp_1");
    }

    #[test]
    fn plan_usage_errors_point_to_chatgpt_settings() {
        let body =
            r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"x"}}"#;
        assert!(http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, body)
            .contains("chatgpt.com/settings/usage"));
        let mut reply = Reply::default();
        let failed = r#"data: {"type":"response.failed","response":{"error":{"code":"subscription_sharing_user_not_eligible"}}}"#;
        let out = reply
            .push(format!("{failed}\n\n").as_bytes())
            .unwrap()
            .unwrap_err();
        assert!(out.contains("can't share its plan"), "{out}");
    }
}
