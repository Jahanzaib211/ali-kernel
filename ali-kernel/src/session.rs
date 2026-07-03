use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::env;

use crate::config::Config;

const DEFAULT_KEY_ENV: &str = "ALI_SESSION_KEY";
const FALLBACK_KEY_ENV: &str = "OH_SECRET_KEY";
const DEFAULT_TTL_SECS: i64 = 3600;
const MAX_TTL_SECS: i64 = 86_400;

/// `passthrough` — caller's tools/tools_choice pass through untouched; gateway native tools suppressed.
/// `native`     — only the gateway's enabled_tools are exposed (legacy default).
/// `hybrid`     — caller's tools merge with native, deduplicated by (name, json-schema fingerprint).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolMode {
    Passthrough,
    Native,
    Hybrid,
}

impl Default for ToolMode {
    fn default() -> Self {
        ToolMode::Native
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    pub sub: String,
    pub tool_mode: ToolMode,
    pub features: Vec<String>,
    pub exp: i64,
    pub iat: i64,
    pub jti: String,
}

/// Sign a JWT using the resolved signing key.
/// Key resolution order: `ALI_SESSION_KEY` env, fall back to `OH_SECRET_KEY`,
/// finally a development fallback string (gated by non-prod check in caller).
pub fn encode_session(
    claims: &SessionClaims,
    key: &[u8],
) -> Result<String, jsonwebtoken::errors::Error> {
    encode(&Header::default(), claims, &EncodingKey::from_secret(key))
}

pub fn decode_session(token: &str, key: &[u8]) -> Result<SessionClaims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::default();
    validation.leeway = 5;
    let data = decode::<SessionClaims>(token, &DecodingKey::from_secret(key), &validation)?;
    Ok(data.claims)
}

/// Resolve the HMAC key the gateway uses to mint and verify session JWTs.
/// Returns `None` when neither `ALI_SESSION_KEY` nor `OH_SECRET_KEY` is set
/// AND the caller has not authorised a development fallback.
pub fn resolve_signing_key(config: &Config) -> Option<Vec<u8>> {
    if let Ok(k) = env::var(DEFAULT_KEY_ENV) {
        if !k.is_empty() {
            return Some(k.into_bytes());
        }
    }
    if let Ok(k) = env::var(FALLBACK_KEY_ENV) {
        if !k.is_empty() {
            return Some(k.into_bytes());
        }
    }
    if config.dev_insecure_auth_ok {
        tracing::warn!(
            "ALI_SESSION_KEY/OH_SECRET_KEY unset; using development fallback signing key. \
             Set ALI_SESSION_KEY for any non-localhost deployment."
        );
        return Some(b"ali-kernel-dev-only-do-not-use-in-prod".to_vec());
    }
    None
}

pub fn clamp_ttl(requested: i64) -> i64 {
    requested.clamp(60, MAX_TTL_SECS)
}

pub fn default_ttl() -> i64 {
    DEFAULT_TTL_SECS
}
