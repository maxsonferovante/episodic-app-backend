use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation, Algorithm};
use serde::{Deserialize, Serialize};

const ACCESS_TOKEN_TTL_SECS: usize = 60 * 60;
const REFRESH_TOKEN_TTL_SECS: usize = 30 * 24 * 60 * 60;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
    pub token_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshClaims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
    pub token_type: String,
}

pub fn encode_access_token(user_id: &str, email: &str, secret: &[u8]) -> Result<String, jsonwebtoken::errors::Error> {
    let now = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        email: email.to_string(),
        exp: now + ACCESS_TOKEN_TTL_SECS,
        iat: now,
        token_type: "access".to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
}

pub fn encode_refresh_token(user_id: &str, email: &str, secret: &[u8]) -> Result<String, jsonwebtoken::errors::Error> {
    let now = chrono::Utc::now().timestamp() as usize;
    let claims = RefreshClaims {
        sub: user_id.to_string(),
        email: email.to_string(),
        exp: now + REFRESH_TOKEN_TTL_SECS,
        iat: now,
        token_type: "refresh".to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
}

pub fn verify_access_token(token: &str, secret: &[u8]) -> Result<Claims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);

    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &validation,
    )?;

    if data.claims.token_type != "access" {
        return Err(jsonwebtoken::errors::Error::from(
            jsonwebtoken::errors::ErrorKind::InvalidToken,
        ));
    }

    Ok(data.claims)
}

pub fn verify_refresh_token(token: &str, secret: &[u8]) -> Result<RefreshClaims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);

    let data = decode::<RefreshClaims>(
        token,
        &DecodingKey::from_secret(secret),
        &validation,
    )?;

    if data.claims.token_type != "refresh" {
        return Err(jsonwebtoken::errors::Error::from(
            jsonwebtoken::errors::ErrorKind::InvalidToken,
        ));
    }

    Ok(data.claims)
}

pub fn extract_user_id(req: &lambda_http::Request) -> Result<String, crate::error::AppError> {
    let header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(crate::error::AppError::AuthenticationRequired)?;

    let token = header
        .strip_prefix("Bearer ")
        .ok_or(crate::error::AppError::AuthenticationRequired)?;

    let secret = std::env::var("JWT_SECRET")
        .map(|s| s.into_bytes())
        .map_err(|_| crate::error::AppError::Internal("JWT_SECRET not set".into()))?;

    let claims = verify_access_token(token, &secret)
        .map_err(|_| crate::error::AppError::AuthenticationRequired)?;

    Ok(claims.sub)
}
