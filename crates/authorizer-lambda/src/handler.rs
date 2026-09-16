use lambda_runtime::{Error, LambdaEvent};
use serde::Deserialize;
use serde_json::{json, Value};

/// API Gateway maps an authorizer error with exactly this message to a 401
/// Unauthorized response (any other message becomes a 500).
const UNAUTHORIZED: &str = "Unauthorized";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizerEvent {
    authorization_token: Option<String>,
    method_arn: String,
}

/// API Gateway caches the authorizer result by token (see the authorizer's
/// `authorizer_result_ttl_in_seconds`), so the policy must allow every method
/// of the API — not just the `methodArn` of the request being authorized.
/// Otherwise the cached policy denies subsequent calls to other routes with a
/// 403 ACCESS_DENIED.
fn allow_resource(method_arn: &str) -> String {
    // arn:aws:execute-api:<region>:<account>:<apiId>/<stage>/<method>/<path>
    let mut parts = method_arn.split(':');
    let base: Vec<&str> = parts.by_ref().take(5).collect();
    let rest = parts.next().unwrap_or("");
    let mut segments = rest.split('/');
    let api_id = segments.next().unwrap_or("*");
    let stage = segments.next().unwrap_or("*");
    format!("{}:{}/{}/*", base.join(":"), api_id, stage)
}

/// API Gateway `TOKEN` authorizer. Validates the HS256 access token and returns
/// an `Allow` policy carrying the user id, so every protected route is
/// authenticated before the backend Lambda runs.
pub async fn handle_request(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let parsed: AuthorizerEvent =
        serde_json::from_value(event.payload).map_err(|_| Error::from(UNAUTHORIZED))?;

    let token = parsed
        .authorization_token
        .as_deref()
        .and_then(|header| header.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| Error::from(UNAUTHORIZED))?;

    let secret = std::env::var("JWT_SECRET")
        .map(String::into_bytes)
        .map_err(|_| Error::from(UNAUTHORIZED))?;

    let claims =
        shared::auth::verify_access_token(token, &secret).map_err(|_| Error::from(UNAUTHORIZED))?;

    Ok(json!({
        "principalId": claims.sub,
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [{
                "Action": "execute-api:Invoke",
                "Effect": "Allow",
                "Resource": allow_resource(&parsed.method_arn),
            }],
        },
        "context": { "userId": claims.sub },
    }))
}
