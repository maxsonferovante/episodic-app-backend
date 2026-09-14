use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GoogleTokenPayload {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
}

fn base_url() -> String {
    std::env::var("GOOGLE_TOKEN_URL")
        .unwrap_or_else(|_| "https://www.googleapis.com/oauth2/v3".to_string())
}

pub async fn validate_google_token(token: &str) -> Result<GoogleTokenPayload, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();
    let resp: serde_json::Value = client
        .get(format!("{}/tokeninfo", base_url()))
        .query(&[("id_token", token)])
        .send()
        .await?
        .json()
        .await?;

    if resp.get("error").is_some() {
        let msg = resp.get("error_description")
            .or(resp.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("Google token validation failed: {}", msg).into());
    }

    let sub = resp["sub"].as_str().unwrap_or("").to_string();
    let email = resp["email"].as_str().unwrap_or("").to_string();

    if sub.is_empty() || email.is_empty() {
        return Err("Google token missing required fields (sub, email)".into());
    }

    Ok(GoogleTokenPayload {
        sub,
        email,
        name: resp["name"].as_str().unwrap_or("").to_string(),
        picture: resp.get("picture").and_then(|v| v.as_str()).map(|s| s.to_string()),
    })
}
