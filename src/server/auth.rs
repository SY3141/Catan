use std::{env, sync::Arc};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;

#[derive(Clone)]
pub struct ClerkAuth {
    decoding_key: Arc<DecodingKey>,
    issuer: String,
    authorized_parties: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ClerkClaims {
    sub: String,
    azp: Option<String>,
}

impl ClerkAuth {
    pub fn optional_from_env() -> Result<Option<Self>, String> {
        let Ok(jwt_key) = env::var("CLERK_JWT_KEY") else {
            return Ok(None);
        };
        if jwt_key.trim().is_empty() {
            return Ok(None);
        }

        Self::from_parts(jwt_key).map(Some)
    }

    pub fn from_env() -> Result<Self, String> {
        let jwt_key = env::var("CLERK_JWT_KEY")
            .map_err(|_| "CLERK_JWT_KEY must be set to the Clerk JWT public key".to_string())?;
        Self::from_parts(jwt_key)
    }

    fn from_parts(jwt_key: String) -> Result<Self, String> {
        let issuer =
            env::var("CLERK_ISSUER").map_err(|_| "CLERK_ISSUER must be set".to_string())?;
        let authorized_parties = parse_authorized_parties(
            &env::var("CLERK_AUTHORIZED_PARTIES")
                .map_err(|_| "CLERK_AUTHORIZED_PARTIES must be set".to_string())?,
        );

        if issuer.trim().is_empty() {
            return Err("CLERK_ISSUER must not be empty".into());
        }
        if authorized_parties.is_empty() {
            return Err("CLERK_AUTHORIZED_PARTIES must include at least one origin".into());
        }

        let pem = jwt_key.replace("\\n", "\n");
        let decoding_key = DecodingKey::from_rsa_pem(pem.as_bytes())
            .map_err(|e| format!("Invalid CLERK_JWT_KEY: {e}"))?;

        Ok(Self {
            decoding_key: Arc::new(decoding_key),
            issuer,
            authorized_parties,
        })
    }

    pub fn verify_user_id(&self, token: &str) -> Result<String, String> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.validate_aud = false;

        let token = token.trim();
        if token.is_empty() {
            return Err("Missing token".into());
        }

        let data = decode::<ClerkClaims>(token, self.decoding_key.as_ref(), &validation)
            .map_err(|e| format!("Invalid Clerk token: {e}"))?;
        let user_id = data.claims.sub.trim();

        if user_id.is_empty() {
            return Err("Clerk token is missing sub".into());
        }
        if !authorized_party_allowed(&self.authorized_parties, data.claims.azp.as_deref()) {
            return Err("Clerk token is from an unauthorized origin".into());
        }

        Ok(user_id.to_string())
    }
}

fn parse_authorized_parties(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|party| !party.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn authorized_party_allowed(authorized_parties: &[String], azp: Option<&str>) -> bool {
    match azp {
        Some(origin) => authorized_parties.iter().any(|party| party == origin),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{authorized_party_allowed, parse_authorized_parties};

    #[test]
    fn parses_comma_separated_authorized_parties() {
        assert_eq!(
            parse_authorized_parties("http://localhost:3000, https://example.com, "),
            vec!["http://localhost:3000", "https://example.com"]
        );
    }

    #[test]
    fn checks_authorized_party_when_token_has_azp() {
        let parties = parse_authorized_parties("http://localhost:3000,https://example.com");

        assert!(authorized_party_allowed(
            &parties,
            Some("https://example.com")
        ));
        assert!(!authorized_party_allowed(
            &parties,
            Some("https://evil.example")
        ));
        assert!(authorized_party_allowed(&parties, None));
    }
}
