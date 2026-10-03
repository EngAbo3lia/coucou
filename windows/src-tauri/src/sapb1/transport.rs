//! Service Layer transport: login, session reuse, TLS.
//!
//! Verified against a live Business One 10.0 FP 3400 server:
//!   * `POST /b1s/v{1,2}/Login` with `{CompanyDB, UserName, Password}` returns
//!     `SessionId` and a `B1SESSION` cookie; the session idles out after 30
//!     minutes.
//!   * The server presents a self-signed certificate, so the connection fails
//!     unless invalid certificates are accepted.
//!   * The first authenticated call after login costs about 5 s, later ones
//!     about 20 ms — one login per cycle, never one per request.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Re-login a minute before the server's 30-minute idle timeout.
const SESSION_TTL: Duration = Duration::from_secs(29 * 60);

/// One company database on one Service Layer host.
#[derive(Clone)]
pub struct Credentials {
    pub base_url: String,
    pub company: String,
    pub user: String,
    pub password: String,
    /// 1 for OData v3, 2 for OData v4. Both are live on FP 3400.
    pub version: u8,
}

impl Credentials {
    fn url(&self, tail: &str) -> String {
        format!(
            "{}/b1s/v{}/{}",
            self.base_url.trim_end_matches('/'),
            self.version,
            tail
        )
    }

    fn key(&self) -> String {
        format!("{}|{}|{}", self.base_url, self.company, self.user)
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap_or_default()
}

/// Session id for one server/company/user, reused until it ages out.
static SESSION: Mutex<Option<(String, String, Instant)>> = Mutex::new(None);

fn cached(key: &str) -> Option<String> {
    let guard = SESSION.lock().ok()?;
    let (stored_key, id, expires) = guard.as_ref()?;
    (stored_key == key && *expires > Instant::now()).then(|| id.clone())
}

fn store(key: &str, id: &str) {
    if let Ok(mut guard) = SESSION.lock() {
        *guard = Some((key.to_string(), id.to_string(), Instant::now() + SESSION_TTL));
    }
}

fn forget() {
    if let Ok(mut guard) = SESSION.lock() {
        *guard = None;
    }
}

async fn login(c: &Credentials) -> Result<String, String> {
    let body = json!({ "CompanyDB": c.company, "UserName": c.user, "Password": c.password });
    let res = client()
        .post(c.url("Login"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("No connection: {e}"))?;
    let status = res.status();
    let value: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(error_message(status.as_u16(), &value));
    }
    match value.get("SessionId").and_then(Value::as_str) {
        Some(id) => Ok(id.to_string()),
        None => Err("Login returned no session id".into()),
    }
}

async fn session(c: &Credentials) -> Result<String, String> {
    let key = c.key();
    if let Some(id) = cached(&key) {
        return Ok(id);
    }
    let id = login(c).await?;
    store(&key, &id);
    Ok(id)
}

/// `GET` a Service Layer path such as `Orders?$top=1`, re-authenticating once
/// if the cached session expired.
pub async fn get(c: &Credentials, tail: &str) -> Result<Value, String> {
    let body = get_raw(c, tail).await?;
    serde_json::from_str(&body).map_err(|e| format!("Bad JSON: {e}"))
}

/// `GET` a path that answers XML rather than JSON, such as `$metadata`.
pub async fn get_text(c: &Credentials, tail: &str) -> Result<String, String> {
    get_raw(c, tail).await
}

async fn get_raw(c: &Credentials, tail: &str) -> Result<String, String> {
    let mut id = session(c).await?;
    for attempt in 0..2 {
        let res = client()
            .get(c.url(tail))
            .header("Cookie", format!("B1SESSION={id}"))
            .send()
            .await
            .map_err(|e| format!("No connection: {e}"))?;
        let status = res.status();
        if status.as_u16() == 401 && attempt == 0 {
            forget();
            id = login(c).await?;
            store(&c.key(), &id);
            continue;
        }
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(error_message(status.as_u16(), &serde_json::from_str(&body).unwrap_or(Value::Null)));
        }
        return Ok(body);
    }
    Err("Session kept expiring".into())
}

/// Service Layer wraps errors as `error.message.value` (v1, OData v3) or
/// `error.message` (v2, OData v4). Both shapes are handled.
fn error_message(status: u16, value: &Value) -> String {
    let detail = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.get("value").and_then(Value::as_str).or_else(|| m.as_str()))
        .unwrap_or("request failed");
    format!("HTTP {status}: {detail}")
}
