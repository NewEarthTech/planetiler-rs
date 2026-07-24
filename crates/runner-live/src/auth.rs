use std::collections::HashMap;

use anyhow::{Context, bail};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

/// Auth config resolution order: explicit disable (local dev only), explicit
/// pool/client/region overrides, else the site's published `config.json` —
/// the same discovery mechanism the SPA uses, so the nano needs zero wiring
/// of its own.
pub struct AuthArgs {
    pub no_auth: bool,
    pub user_pool_id: Option<String>,
    pub client_id: Option<String>,
    pub auth_region: Option<String>,
    pub config_url: String,
}

pub enum Auth {
    /// Requests pass unauthenticated. Refused unless asked for by flag.
    Disabled,
    Cognito(CognitoAuth),
}

/// Verifies the SPA's Cognito id token (RS256) against the user pool JWKS.
/// Keys are fetched at boot (fail fast on a bad pool) and refreshed once on
/// an unknown `kid`, which covers rotation.
pub struct CognitoAuth {
    issuer: String,
    audience: String,
    jwks_url: String,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, DecodingKey>>,
}

#[derive(Deserialize)]
struct SiteConfig {
    auth: SiteAuth,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SiteAuth {
    user_pool_id: String,
    client_id: String,
    region: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
    #[serde(default)]
    kty: String,
}

pub async fn resolve(args: AuthArgs) -> anyhow::Result<Auth> {
    if args.no_auth {
        tracing::warn!("auth DISABLED by flag — local dev only");
        return Ok(Auth::Disabled);
    }
    let (pool, client, region) = match (args.user_pool_id, args.client_id, args.auth_region) {
        (Some(p), Some(c), Some(r)) => (p, c, r),
        (None, None, None) => {
            let config: SiteConfig = reqwest::get(&args.config_url)
                .await
                .and_then(reqwest::Response::error_for_status)
                .with_context(|| format!("fetching {}", args.config_url))?
                .json()
                .await
                .context("parsing site config.json")?;
            (
                config.auth.user_pool_id,
                config.auth.client_id,
                config.auth.region,
            )
        }
        _ => bail!("set all of --user-pool-id/--client-id/--auth-region, or none"),
    };
    let auth = CognitoAuth::new(&pool, &client, &region);
    let fetched = auth.refresh_keys().await.context("fetching JWKS")?;
    tracing::info!(pool = %pool, keys = fetched, "cognito auth ready");
    Ok(Auth::Cognito(auth))
}

impl CognitoAuth {
    pub fn new(user_pool_id: &str, client_id: &str, region: &str) -> Self {
        let issuer = format!("https://cognito-idp.{region}.amazonaws.com/{user_pool_id}");
        Self {
            jwks_url: format!("{issuer}/.well-known/jwks.json"),
            issuer,
            audience: client_id.to_string(),
            http: reqwest::Client::new(),
            keys: RwLock::new(HashMap::new()),
        }
    }

    pub async fn verify(&self, token: &str) -> anyhow::Result<()> {
        let header = decode_header(token).context("malformed token header")?;
        let kid = header.kid.context("token has no kid")?;
        if !self.keys.read().await.contains_key(&kid) {
            // Unknown kid: refresh once — either rotation, or a bad token.
            self.refresh_keys().await.context("refreshing JWKS")?;
        }
        let keys = self.keys.read().await;
        let key = keys.get(&kid).context("token kid not in JWKS")?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);
        decode::<serde_json::Value>(token, key, &validation).context("token rejected")?;
        Ok(())
    }

    async fn refresh_keys(&self) -> anyhow::Result<usize> {
        let jwks: Jwks = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)?
            .json()
            .await?;
        let mut keys = self.keys.write().await;
        keys.clear();
        for jwk in &jwks.keys {
            if !jwk.kty.is_empty() && jwk.kty != "RSA" {
                continue;
            }
            let key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
                .with_context(|| format!("bad JWK {}", jwk.kid))?;
            keys.insert(jwk.kid.clone(), key);
        }
        Ok(keys.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn garbage_token_is_rejected_before_any_network() {
        let auth = CognitoAuth::new("us-east-1_TEST", "client123", "us-east-1");
        assert!(auth.verify("not-a-jwt").await.is_err());
    }

    #[tokio::test]
    async fn mismatched_overrides_are_refused() {
        let result = resolve(AuthArgs {
            no_auth: false,
            user_pool_id: Some("pool".into()),
            client_id: None,
            auth_region: None,
            config_url: "http://unused.invalid/config.json".into(),
        })
        .await;
        assert!(result.is_err());
    }
}
