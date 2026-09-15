//! share-musics account bridge for Junction.
//!
//! When the DJ signs in with a Google account registered in share-musics, the
//! Junction panel uses the share-musics Worker as a mailbox for the native
//! invite/response packets instead of the clipboard. Tokens never reach the
//! webview: the refresh token lives in the OS credential store, the short-lived
//! ID token in memory, and the webview may only call an allowlisted set of
//! Worker actions through `share_musics_request`.

use base64::Engine as _;
use reqwest::Url;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};
use tauri_plugin_shell::ShellExt;

/// PlumDeck Lite Worker, embedded at build time from the
/// `PLUMDECK_SHARE_MUSICS_URL` GitHub secret. Builds without it keep Junction's
/// manual flow and hide PlumDeck Lite sign-in.
const BUILD_SERVICE_URL: Option<&str> = option_env!("PLUMDECK_SHARE_MUSICS_URL");
const NOT_CONFIGURED: &str = "このビルドではPlumDeck Liteとの連携が設定されていません";
/// Loopback path the Worker accepts as a desktop `return_to` (RFC 8252).
const AUTH_PATH: &str = "/plumdeck-junction-auth";
const CREDENTIAL_SERVICE: &str = "plumdeck.share-musics";
const CREDENTIAL_ACCOUNT: &str = "junction-refresh-token";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CALLBACK_HEADER_BYTES: usize = 16 * 1024;
const MAX_CALLBACK_BODY_BYTES: usize = 32 * 1024;
/// Worker actions reachable from the webview, and whether each one is a POST.
const ACTIONS: &[(&str, bool)] = &[
    ("listJunctionSessions", false),
    ("getJunctionSession", false),
    ("createJunctionSession", true),
    ("joinJunctionSession", true),
    ("approveJunctionMember", true),
    ("postJunctionSignal", true),
    ("setJunctionOwner", true),
    ("leaveJunctionSession", true),
];
const NOT_SIGNED_IN: &str = "PlumDeck Liteにログインしてください";
const NOT_REGISTERED: &str =
    "このGoogleアカウントはPlumDeck Liteに登録されていません。従来の招待・返答で接続してください";

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ApiError {
    status: u16,
    message: String,
}

impl ApiError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    fn network(error: reqwest::Error) -> Self {
        let message = if error.is_timeout() {
            "PlumDeck Liteの応答がありません。通信環境を確認してください"
        } else {
            "PlumDeck Liteに接続できません。通信環境を確認してください"
        };
        Self::new(503, message)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    /// False when this build has no PlumDeck Lite service configured.
    available: bool,
    signed_in: bool,
    email: Option<String>,
    service_url: String,
}

#[derive(Default)]
pub struct ShareMusics {
    id_token: Mutex<Option<String>>,
    /// Cached copy of the credential-store value; also the only copy when the
    /// store is unavailable, in which case sign-in lasts for this launch.
    refresh_token: Mutex<Option<String>>,
    login_active: AtomicBool,
    login_cancel: Arc<AtomicBool>,
    /// Bumped by sign-out; a refresh that started before it never stores its token.
    generation: AtomicU64,
    http: reqwest::Client,
}

struct LoginGuard<'a>(&'a AtomicBool);
impl Drop for LoginGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn service_url() -> Option<String> {
    // Development builds may also point at `wrangler dev` at run time; release
    // builds only use the URL fixed at build time, so an environment variable
    // cannot redirect tokens.
    if cfg!(debug_assertions) {
        if let Some(url) = std::env::var("PLUMDECK_SHARE_MUSICS_URL").ok().and_then(|value| valid_service_url(&value)) {
            return Some(url);
        }
    }
    BUILD_SERVICE_URL.and_then(valid_service_url)
}

fn require_service_url() -> Result<String, ApiError> {
    service_url().ok_or_else(|| ApiError::new(503, NOT_CONFIGURED))
}

fn valid_service_url(value: &str) -> Option<String> {
    let url = Url::parse(value.trim()).ok()?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    let allowed = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    (allowed && url.username().is_empty() && url.password().is_none() && url.query().is_none())
        .then(|| url.as_str().trim_end_matches('/').to_string())
}

fn action_method(action: &str) -> Option<bool> {
    ACTIONS.iter().find(|(name, _)| *name == action).map(|(_, post)| *post)
}

/// Unverified ID token claims, used only for display and refresh timing. The
/// Worker verifies every token with Google before trusting it.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn token_email(token: &str) -> Option<String> {
    jwt_claims(token)?.get("email")?.as_str().map(str::to_string)
}

fn token_fresh(token: &str) -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    jwt_claims(token)
        .and_then(|claims| claims.get("exp").and_then(Value::as_u64))
        .is_some_and(|exp| exp > now + 60)
}

fn random_hex(bytes: usize) -> Result<String, ApiError> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| ApiError::new(500, "ログイン用の乱数を生成できません"))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left.bytes().zip(right.bytes()).fold(0_u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

fn credential_entry() -> Option<keyring::Entry> {
    keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_ACCOUNT).ok()
}

impl ShareMusics {
    fn stored_refresh_token(&self) -> Option<String> {
        let mut cached = self.refresh_token.lock().ok()?;
        if cached.is_none() {
            *cached = credential_entry().and_then(|entry| entry.get_password().ok());
        }
        cached.clone()
    }

    fn store_refresh_token(&self, token: &str) {
        if let Ok(mut cached) = self.refresh_token.lock() {
            *cached = Some(token.to_string());
        }
        if let Some(entry) = credential_entry() {
            let _ = entry.set_password(token);
        }
    }

    fn set_id_token(&self, token: Option<String>) {
        if let Ok(mut current) = self.id_token.lock() {
            *current = token;
        }
    }

    fn current_id_token(&self) -> Option<String> {
        self.id_token.lock().ok()?.clone()
    }

    fn forget(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.set_id_token(None);
        if let Ok(mut cached) = self.refresh_token.lock() {
            *cached = None;
        }
        if let Some(entry) = credential_entry() {
            let _ = entry.delete_credential();
        }
    }

    async fn refresh_id_token(&self) -> Result<String, ApiError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let refresh = self.stored_refresh_token().ok_or_else(|| ApiError::new(401, NOT_SIGNED_IN))?;
        let url = Url::parse_with_params(&format!("{}/auth/refresh", require_service_url()?), &[("refresh_token", refresh.as_str())])
            .map_err(|_| ApiError::new(500, "PlumDeck Liteの接続先が不正です"))?;
        let response = self.http.get(url).timeout(REQUEST_TIMEOUT).send().await.map_err(ApiError::network)?;
        let status = response.status();
        if status.as_u16() == 401 {
            // Revoked or expired refresh token (Google invalid_grant): sign in again.
            // Any other failure is temporary and keeps the stored token.
            if self.generation.load(Ordering::SeqCst) == generation {
                self.forget();
            }
            return Err(ApiError::new(401, "PlumDeck Liteのログインが切れました。もう一度ログインしてください"));
        }
        let body = json_body(response).await?;
        let token = body.get("id_token").and_then(Value::as_str).filter(|_| status.is_success());
        let token = token.ok_or_else(|| ApiError::new(503, "PlumDeck Liteのログインを更新できません。しばらくしてからお試しください"))?.to_string();
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(ApiError::new(401, NOT_SIGNED_IN));
        }
        self.set_id_token(Some(token.clone()));
        Ok(token)
    }

    async fn id_token(&self) -> Result<String, ApiError> {
        match self.current_id_token() {
            Some(token) if token_fresh(&token) => Ok(token),
            _ => self.refresh_id_token().await,
        }
    }

    async fn call(&self, action: &str, query: &HashMap<String, String>, body: Option<&Value>) -> Result<Value, ApiError> {
        let post = action_method(action).ok_or_else(|| ApiError::new(400, "PlumDeck Liteで許可されていない操作です"))?;
        let url = Url::parse_with_params(&format!("{}/api/{action}", require_service_url()?), query.iter())
            .map_err(|_| ApiError::new(500, "PlumDeck Liteの接続先が不正です"))?;
        for attempt in 0..2 {
            let token = if attempt == 0 { self.id_token().await? } else { self.refresh_id_token().await? };
            let request = if post {
                let payload = serde_json::to_vec(body.unwrap_or(&Value::Object(Default::default())))
                    .map_err(|_| ApiError::new(400, "送信内容が不正です"))?;
                self.http.post(url.clone()).header(reqwest::header::CONTENT_TYPE, "application/json").body(payload)
            } else {
                self.http.get(url.clone())
            };
            let response = request.bearer_auth(token).timeout(REQUEST_TIMEOUT).send().await.map_err(ApiError::network)?;
            let status = response.status().as_u16();
            if status == 401 && attempt == 0 {
                continue;
            }
            if status == 401 {
                // A freshly refreshed token was still refused: not on the whitelist.
                self.forget();
                return Err(ApiError::new(401, NOT_REGISTERED));
            }
            let body = json_body(response).await.unwrap_or(Value::Null);
            if status == 503 {
                return Err(ApiError::new(503, "PlumDeck LiteがGoogleのログインを確認できません。しばらくしてからお試しください"));
            }
            if !(200..300).contains(&status) {
                let message = body.get("error").and_then(Value::as_str).unwrap_or("PlumDeck Liteで処理できませんでした");
                return Err(ApiError::new(status, message));
            }
            return Ok(body);
        }
        Err(ApiError::new(401, NOT_REGISTERED))
    }

    async fn status(&self) -> Result<AccountStatus, ApiError> {
        let Some(service_url) = service_url() else {
            return Ok(AccountStatus { available: false, signed_in: false, email: None, service_url: String::new() });
        };
        if self.current_id_token().is_none() && self.stored_refresh_token().is_none() {
            return Ok(AccountStatus { available: true, signed_in: false, email: None, service_url });
        }
        match self.id_token().await {
            Ok(token) => Ok(AccountStatus { available: true, signed_in: true, email: token_email(&token), service_url }),
            Err(error) if error.status == 401 => Ok(AccountStatus { available: true, signed_in: false, email: None, service_url }),
            Err(error) => Err(error),
        }
    }
}

async fn json_body(response: reqwest::Response) -> Result<Value, ApiError> {
    let bytes = response.bytes().await.map_err(ApiError::network)?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::new(502, "PlumDeck Liteの応答が不正です"))
}

#[tauri::command]
pub async fn share_musics_status(state: State<'_, ShareMusics>) -> Result<AccountStatus, ApiError> {
    state.status().await
}

#[tauri::command]
pub async fn share_musics_login(app: AppHandle, state: State<'_, ShareMusics>) -> Result<AccountStatus, ApiError> {
    if state.login_active.swap(true, Ordering::SeqCst) {
        return Err(ApiError::new(409, "ブラウザでログイン手続き中です。完了するか、中止してください"));
    }
    let _guard = LoginGuard(&state.login_active);
    state.login_cancel.store(false, Ordering::SeqCst);
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.set_nonblocking(true).map(|_| listener))
        .map_err(|_| ApiError::new(500, "ログインの受付を開始できません"))?;
    let port = listener.local_addr().map_err(|_| ApiError::new(500, "ログインの受付を開始できません"))?.port();
    let nonce = random_hex(24)?;
    let return_to = format!("http://127.0.0.1:{port}{AUTH_PATH}?state={nonce}");
    let login_url = Url::parse_with_params(&format!("{}/auth/login", require_service_url()?), &[("return_to", return_to.as_str())])
        .map_err(|_| ApiError::new(500, "PlumDeck Liteの接続先が不正です"))?;
    #[allow(deprecated)]
    app.shell()
        .open(login_url.to_string(), None)
        .map_err(|_| ApiError::new(500, "ブラウザを開けません"))?;
    let cancel = state.login_cancel.clone();
    let tokens = tauri::async_runtime::spawn_blocking(move || wait_for_callback(&listener, &nonce, &cancel))
        .await
        .map_err(|_| ApiError::new(500, "ログインを完了できません"))??;
    let id_token = tokens.get("id_token").filter(|token| !token.is_empty()).cloned()
        .ok_or_else(|| ApiError::new(400, "ログイン情報を受け取れませんでした"))?;
    if let Some(refresh) = tokens.get("refresh_token").filter(|token| !token.is_empty()) {
        state.store_refresh_token(refresh);
    }
    state.set_id_token(Some(id_token));
    // Signing in alone proves only a Google account; the whitelist is checked here.
    state.call("listJunctionSessions", &HashMap::new(), None).await?;
    state.status().await
}

#[tauri::command]
pub fn share_musics_cancel_login(state: State<'_, ShareMusics>) {
    state.login_cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
pub fn share_musics_logout(state: State<'_, ShareMusics>) {
    state.login_cancel.store(true, Ordering::SeqCst);
    state.forget();
}

#[tauri::command]
pub async fn share_musics_request(
    state: State<'_, ShareMusics>,
    action: String,
    query: Option<HashMap<String, String>>,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    state.call(&action, &query.unwrap_or_default(), body.as_ref()).await
}

fn wait_for_callback(listener: &TcpListener, nonce: &str, cancel: &AtomicBool) -> Result<HashMap<String, String>, ApiError> {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(ApiError::new(499, "ログインを中止しました"));
        }
        if Instant::now() >= deadline {
            return Err(ApiError::new(408, "ログインが時間内に完了しませんでした。もう一度お試しください"));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Some(tokens) = handle_callback(&mut stream, nonce) {
                    return Ok(tokens);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

struct CallbackRequest {
    method: String,
    path: String,
    state: String,
    body: Vec<u8>,
}

fn read_callback(stream: &mut TcpStream) -> Option<CallbackRequest> {
    let mut received = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok().filter(|read| *read > 0)?;
        received.extend_from_slice(&chunk[..read]);
        if let Some(index) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if received.len() > MAX_CALLBACK_HEADER_BYTES {
            return None;
        }
    };
    let header = std::str::from_utf8(&received[..header_end]).ok()?;
    let mut lines = header.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let target = Url::parse(&format!("http://127.0.0.1{}", request_line.next()?)).ok()?;
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_CALLBACK_BODY_BYTES {
        return None;
    }
    while received.len() < header_end + length {
        let read = stream.read(&mut chunk).ok().filter(|read| *read > 0)?;
        received.extend_from_slice(&chunk[..read]);
    }
    let state = target.query_pairs().find(|(name, _)| name == "state").map(|(_, value)| value.into_owned()).unwrap_or_default();
    Some(CallbackRequest { method, path: target.path().to_string(), state, body: received[header_end..header_end + length].to_vec() })
}

/// Serves the one-page relay that moves the token fragment (never sent to a
/// server by the browser) to this listener, then accepts that POST.
fn handle_callback(stream: &mut TcpStream, nonce: &str) -> Option<HashMap<String, String>> {
    let _ = stream.set_nonblocking(false);
    // Short, so an idle browser preconnect cannot hold the single listener for long.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let Some(request) = read_callback(stream) else {
        let _ = write_response(stream, 400, "text/plain; charset=utf-8", "", b"Bad Request");
        return None;
    };
    if !constant_time_equal(&request.state, nonce) {
        let _ = write_response(stream, 404, "text/plain; charset=utf-8", "", b"Not Found");
        return None;
    }
    let complete_path = format!("{AUTH_PATH}/complete");
    if request.method == "GET" && request.path == AUTH_PATH {
        let script_nonce = random_hex(16).ok()?;
        let page = relay_page(nonce, &script_nonce);
        let csp = format!("default-src 'none'; script-src 'nonce-{script_nonce}'; connect-src 'self'; style-src 'unsafe-inline'");
        let _ = write_response(stream, 200, "text/html; charset=utf-8", &csp, page.as_bytes());
        return None;
    }
    if request.method == "POST" && request.path == complete_path {
        let tokens = parse_fragment(&request.body);
        if tokens.get("id_token").is_some_and(|token| !token.is_empty()) {
            let _ = write_response(stream, 200, "text/plain; charset=utf-8", "", b"OK");
            return Some(tokens);
        }
        let _ = write_response(stream, 400, "text/plain; charset=utf-8", "", b"Bad Request");
        return None;
    }
    let _ = write_response(stream, 404, "text/plain; charset=utf-8", "", b"Not Found");
    None
}

fn parse_fragment(body: &[u8]) -> HashMap<String, String> {
    let Ok(text) = std::str::from_utf8(body) else { return HashMap::new() };
    Url::parse(&format!("http://127.0.0.1/?{}", text.trim()))
        .map(|url| url.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect())
        .unwrap_or_default()
}

fn relay_page(state: &str, script_nonce: &str) -> String {
    format!(
        r#"<!doctype html><html lang="ja"><meta charset="utf-8"><title>plumdeck</title>
<body style="font-family:system-ui,sans-serif;padding:48px;line-height:1.7">
<p id="message">plumdeckへログイン情報を渡しています…</p>
<script nonce="{script_nonce}">
const fragment = location.hash.slice(1);
history.replaceState(null, '', location.pathname);
const message = document.getElementById('message');
if (!fragment) {{
  message.textContent = 'ログイン情報を受け取れませんでした。plumdeckに戻り、もう一度ログインしてください。';
}} else {{
  fetch('{AUTH_PATH}/complete?state={state}', {{method: 'POST', headers: {{'Content-Type': 'text/plain'}}, body: fragment}})
    .then((response) => {{ message.textContent = response.ok ? 'ログインしました。このタブを閉じてplumdeckに戻ってください。' : 'ログインを完了できませんでした。plumdeckに戻り、もう一度お試しください。'; }})
    .catch(() => {{ message.textContent = 'plumdeckに接続できませんでした。もう一度お試しください。'; }});
}}
</script>"#
    )
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, csp: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let csp = if csp.is_empty() { String::new() } else { format!("Content-Security-Policy: {csp}\r\n") };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{csp}Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(claims: &str) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", engine.encode(r#"{"alg":"RS256"}"#), engine.encode(claims))
    }

    #[test]
    fn only_junction_actions_are_reachable() {
        assert_eq!(action_method("getJunctionSession"), Some(false));
        assert_eq!(action_method("postJunctionSignal"), Some(true));
        for action in ["getTracks", "putPlaylistTree", "getDjMeta", "../auth/refresh", ""] {
            assert_eq!(action_method(action), None, "{action}");
        }
    }

    #[test]
    fn service_override_accepts_only_https_or_loopback() {
        assert_eq!(valid_service_url("http://127.0.0.1:8787/").as_deref(), Some("http://127.0.0.1:8787"));
        assert_eq!(valid_service_url("https://example.workers.dev").as_deref(), Some("https://example.workers.dev"));
        assert_eq!(valid_service_url("http://example.com"), None);
        // Assembled so secret scanners do not mistake the fixture for a credential.
        assert_eq!(valid_service_url(&["https://user", ":pass@example.com"].concat()), None);
    }

    #[test]
    fn id_token_claims_drive_display_and_refresh() {
        assert_eq!(token_email(&token(r#"{"email":"dj@example.com","exp":1}"#)).as_deref(), Some("dj@example.com"));
        assert!(!token_fresh(&token(r#"{"exp":1}"#)));
        assert!(token_fresh(&token(r#"{"exp":99999999999}"#)));
        assert!(!token_fresh("not-a-token"));
    }

    #[test]
    fn fragment_is_parsed_as_form_pairs() {
        let tokens = parse_fragment(b"id_token=a.b.c&refresh_token=1%2F2");
        assert_eq!(tokens.get("id_token").map(String::as_str), Some("a.b.c"));
        assert_eq!(tokens.get("refresh_token").map(String::as_str), Some("1/2"));
    }

    #[test]
    fn loopback_relay_requires_state_and_returns_tokens() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let send = |request: String| {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream.write_all(request.as_bytes()).unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                response
            };
            let wrong = send(format!("GET {AUTH_PATH}?state=wrong HTTP/1.1\r\nHost: x\r\n\r\n"));
            let page = send(format!("GET {AUTH_PATH}?state=abc HTTP/1.1\r\nHost: x\r\n\r\n"));
            let body = "id_token=x.y.z&refresh_token=r";
            let done = send(format!("POST {AUTH_PATH}/complete?state=abc HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
            (wrong, page, done)
        });
        let cancel = AtomicBool::new(false);
        listener.set_nonblocking(true).unwrap();
        let tokens = wait_for_callback(&listener, "abc", &cancel).unwrap();
        let (wrong, page, done) = client.join().unwrap();
        assert!(wrong.starts_with("HTTP/1.1 404"));
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("script-src 'nonce-"));
        assert!(done.starts_with("HTTP/1.1 200"));
        assert_eq!(tokens.get("id_token").map(String::as_str), Some("x.y.z"));
        assert_eq!(tokens.get("refresh_token").map(String::as_str), Some("r"));
    }
}
