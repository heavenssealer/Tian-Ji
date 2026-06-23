//! Anthropic subscription login (Claude Pro/Max) via OAuth 2.0 + PKCE — the same flow the Claude
//! Code CLI and `ant auth login` use. Connecting an account here lets turns bill the operator's
//! subscription instead of API credits.
//!
//! Tokens live in the OS keychain (never on disk/DB, like every other secret — DESIGN.md §9.2).
//! The adapter in `tianji-llm` only ever receives a bearer string via [`KeychainOauthSource`];
//! all OAuth/keychain wire details stay confined here.

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::{AppError, AppResult};

/// Public OAuth client id for the Claude Code/subscription flow (not a secret).
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Keychain entry holding the JSON-serialized [`OauthTokens`].
const OAUTH_PROVIDER: &str = "anthropic_oauth";

/// Persisted token set. `expires_at` is an absolute unix timestamp (seconds) so we can decide when
/// to refresh without trusting wall-clock deltas across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OauthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

/// In-flight login: the PKCE verifier + `state` we generated in [`begin`], needed to complete the
/// exchange after the operator pastes back the authorization code. Held in `AppState`, not the
/// keychain — it is single-use and short-lived.
pub struct PendingOauth {
    pub verifier: String,
    pub state: String,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 32 bytes of CSPRNG output, base64url-encoded — used for both the PKCE verifier and `state`.
fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    b64url(&buf)
}

fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// Start a login: build the browser authorization URL and return it alongside the [`PendingOauth`]
/// the caller must hold until [`exchange_code`].
pub fn begin() -> (String, PendingOauth) {
    let verifier = random_token();
    let state = random_token();
    let challenge = pkce_challenge(&verifier);

    let mut url = url::Url::parse(AUTHORIZE_URL).expect("static authorize url is valid");
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);

    (url.to_string(), PendingOauth { verifier, state })
}

/// Exchange the pasted authorization code for tokens. The manual-redirect flow returns the code as
/// `CODE#STATE`; we accept either form and send the verifier we stashed in [`begin`].
pub async fn exchange_code(code_input: &str, pending: &PendingOauth) -> Result<OauthTokens, String> {
    let code = code_input
        .trim()
        .split_once('#')
        .map(|(c, _)| c.trim())
        .unwrap_or_else(|| code_input.trim());

    let body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": pending.state,
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": pending.verifier,
    });
    post_token(&body).await
}

/// Trade a refresh token for a fresh access token. Some responses omit a new refresh token; in
/// that case we keep the existing one so the session survives.
pub async fn refresh(refresh_token: &str) -> Result<OauthTokens, String> {
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });
    let mut tokens = post_token(&body).await?;
    if tokens.refresh_token.is_empty() {
        tokens.refresh_token = refresh_token.to_string();
    }
    Ok(tokens)
}

async fn post_token(body: &Value) -> Result<OauthTokens, String> {
    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .json(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("token endpoint {status}: {text}"));
    }

    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or("token response missing access_token")?
        .to_string();
    let refresh_token = v["refresh_token"].as_str().unwrap_or("").to_string();
    let expires_in = v["expires_in"].as_u64().unwrap_or(3600);

    Ok(OauthTokens { access_token, refresh_token, expires_at: now() + expires_in })
}

// ── keychain persistence (with an in-memory cache) ─────────────────────────────

// Process-wide token cache. Reading the OS keychain prompts for the keychain password on macOS
// when the app binary isn't trusted, and `access_token()` runs on EVERY LLM call — so an agentic
// loop triggered a flurry of prompts "every few minutes". We read the keychain at most once, then
// serve from memory; only a refresh or an explicit connect/disconnect touches the keychain again.
// `loaded` distinguishes "cache is authoritative" from "never read yet".
static TOKEN_CACHE: std::sync::Mutex<(bool, Option<OauthTokens>)> =
    std::sync::Mutex::new((false, None));

pub fn store_tokens(tokens: &OauthTokens) -> AppResult<()> {
    let json = serde_json::to_string(tokens)
        .map_err(|e| AppError::Message(format!("serialize tokens: {e}")))?;
    crate::secrets::set_api_key(OAUTH_PROVIDER, &json)?;
    *TOKEN_CACHE.lock().unwrap() = (true, Some(tokens.clone()));
    Ok(())
}

pub fn load_tokens() -> AppResult<Option<OauthTokens>> {
    {
        let cache = TOKEN_CACHE.lock().unwrap();
        if cache.0 {
            return Ok(cache.1.clone());
        }
    }
    let parsed = match crate::secrets::get_api_key(OAUTH_PROVIDER)? {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(&s).ok(),
        _ => None,
    };
    *TOKEN_CACHE.lock().unwrap() = (true, parsed.clone());
    Ok(parsed)
}

/// Forget the subscription (clears the stored tokens — turns fall back to the API key, if set).
pub fn clear_tokens() -> AppResult<()> {
    crate::secrets::set_api_key(OAUTH_PROVIDER, "")?;
    *TOKEN_CACHE.lock().unwrap() = (true, None);
    Ok(())
}

// ── token source for the LLM adapter ──────────────────────────────────────────

/// Hands the Claude adapter a valid access token each turn, refreshing (and re-persisting) it when
/// it is within a minute of expiry.
pub struct KeychainOauthSource;

#[async_trait::async_trait]
impl tianji_llm::TokenSource for KeychainOauthSource {
    async fn access_token(&self) -> Result<String, tianji_llm::LlmError> {
        let tokens = load_tokens()
            .map_err(|e| tianji_llm::LlmError::Provider(e.to_string()))?
            .ok_or_else(|| {
                tianji_llm::LlmError::Provider("no Anthropic subscription connected".to_string())
            })?;

        if tokens.expires_at <= now() + 60 {
            let refreshed = refresh(&tokens.refresh_token).await.map_err(|e| {
                tianji_llm::LlmError::Provider(format!("subscription token refresh failed: {e}"))
            })?;
            store_tokens(&refreshed)
                .map_err(|e| tianji_llm::LlmError::Provider(e.to_string()))?;
            return Ok(refreshed.access_token);
        }
        Ok(tokens.access_token)
    }
}
