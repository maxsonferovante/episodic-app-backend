use http::StatusCode;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Authentication required")]
    AuthenticationRequired,

    #[error("Invalid credentials")]
    InvalidCredentials,

    #[error("Series not found")]
    SeriesNotFound,

    #[error("Episode not found")]
    EpisodeNotFound,

    #[error("Episode not released yet")]
    EpisodeNotReleased,

    #[error("Already in library")]
    AlreadyInLibrary,

    #[error("Not in library")]
    NotInLibrary,

    #[error("TMDB unavailable")]
    TmdbUnavailable,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &str {
        match self {
            Self::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            Self::InvalidCredentials => "INVALID_CREDENTIALS",
            Self::SeriesNotFound => "SERIES_NOT_FOUND",
            Self::EpisodeNotFound => "EPISODE_NOT_FOUND",
            Self::EpisodeNotReleased => "EPISODE_NOT_RELEASED",
            Self::AlreadyInLibrary => "ALREADY_IN_LIBRARY",
            Self::NotInLibrary => "NOT_IN_LIBRARY",
            Self::TmdbUnavailable => "TMDB_UNAVAILABLE",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::AuthenticationRequired => StatusCode::UNAUTHORIZED,
            Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::SeriesNotFound => StatusCode::NOT_FOUND,
            Self::EpisodeNotFound => StatusCode::NOT_FOUND,
            Self::EpisodeNotReleased => StatusCode::FORBIDDEN,
            Self::AlreadyInLibrary => StatusCode::CONFLICT,
            Self::NotInLibrary => StatusCode::NOT_FOUND,
            Self::TmdbUnavailable => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let body = json!({
            "error": {
                "code": self.code(),
                "message": self.to_string()
            }
        });
        body.serialize(serializer)
    }
}

pub fn cors_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Headers", "Content-Type,Authorization"),
        ("Access-Control-Allow-Methods", "GET,POST,PUT,DELETE,OPTIONS"),
    ]
}

pub fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
) -> http::Response<lambda_http::Body> {
    let body = json!({ "error": { "code": code, "message": message } });
    let mut resp = http::Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(lambda_http::Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();
    add_cors(&mut resp);
    resp
}

pub fn app_error_response(err: AppError) -> http::Response<lambda_http::Body> {
    error_response(err.status_code(), err.code(), &err.to_string())
}

pub fn add_cors(resp: &mut http::Response<lambda_http::Body>) {
    let headers = resp.headers_mut();
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert("Access-Control-Allow-Headers", "Content-Type,Authorization".parse().unwrap());
    headers.insert("Access-Control-Allow-Methods", "GET,POST,PUT,DELETE,OPTIONS".parse().unwrap());
}
