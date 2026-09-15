pub mod series {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Series {
        pub id: String,
        pub tmdb_id: i64,
        pub imdb_id: Option<String>,
        pub name: String,
        pub original_name: String,
        pub overview: String,
        pub poster_path: Option<String>,
        pub backdrop_path: Option<String>,
        pub first_air_date: Option<String>,
        pub last_air_date: Option<String>,
        pub status: String,
        pub number_of_seasons: i32,
        pub number_of_episodes: i32,
        pub created_at: String,
        pub updated_at: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CatalogSeries {
        pub id: i64,
        pub name: String,
        pub poster_path: Option<String>,
        pub first_air_date: Option<String>,
    }

    #[derive(Debug, Serialize)]
    pub struct WatchProviders {
        pub flatrate: Option<Vec<Provider>>,
        pub rent: Option<Vec<Provider>>,
        pub buy: Option<Vec<Provider>>,
        pub free: Option<Vec<Provider>>,
    }

    #[derive(Debug, Serialize)]
    pub struct Provider {
        pub provider_id: i64,
        pub provider_name: String,
        pub logo_path: Option<String>,
    }
}

pub mod season {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Season {
        pub id: String,
        pub series_id: String,
        pub tmdb_id: Option<i64>,
        pub season_number: i32,
        pub name: String,
        pub overview: Option<String>,
        pub poster_path: Option<String>,
        pub air_date: Option<String>,
        pub episode_count: i32,
    }
}

pub mod episode {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Episode {
        pub id: String,
        pub series_id: String,
        pub season_id: String,
        pub tmdb_id: Option<i64>,
        pub episode_number: i32,
        pub name: String,
        pub overview: Option<String>,
        pub still_path: Option<String>,
        pub air_date: Option<String>,
        pub runtime: Option<i32>,
        pub vote_average: Option<f64>,
        /// Per-user watch status, merged when the caller is authenticated.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub status: Option<String>,
    }
}

pub mod user {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct User {
        pub id: String,
        pub email: String,
        pub name: String,
        pub avatar_url: Option<String>,
        pub provider: String,
        pub created_at: String,
        pub updated_at: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct GoogleLoginRequest {
        pub id_token: String,
    }

    #[derive(Debug, Serialize)]
    pub struct AuthResponse {
        pub user: User,
        pub access_token: String,
        pub refresh_token: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RefreshRequest {
        pub refresh_token: String,
    }

    #[derive(Debug, Serialize)]
    pub struct RefreshResponse {
        pub access_token: String,
    }
}

pub mod library {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct LibraryItem {
        pub id: String,
        pub user_id: String,
        pub series_id: String,
        pub added_at: String,
    }
}

pub mod progress {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "UPPERCASE")]
    pub enum WatchStatus {
        Unwatched,
        Watched,
        Upcoming,
    }

    impl WatchStatus {
        /// Canonical DynamoDB string for this status.
        pub const fn as_str(&self) -> &'static str {
            match self {
                WatchStatus::Watched => crate::enums::watch_status::WATCHED,
                WatchStatus::Unwatched => crate::enums::watch_status::UNWATCHED,
                WatchStatus::Upcoming => crate::enums::watch_status::UPCOMING,
            }
        }

        /// Parse a status stored in DynamoDB, defaulting to `Unwatched`.
        pub fn from_db(value: &str) -> WatchStatus {
            match value {
                crate::enums::watch_status::WATCHED => WatchStatus::Watched,
                crate::enums::watch_status::UPCOMING => WatchStatus::Upcoming,
                _ => WatchStatus::Unwatched,
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct WatchProgress {
        pub user_id: String,
        pub series_id: String,
        pub season_number: i32,
        pub episode_number: i32,
        pub status: WatchStatus,
        pub watched_at: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct WatchEvent {
        pub user_id: String,
        pub episode_id: String,
        pub event_type: String,
        pub occurred_at: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct EpisodeProgress {
        pub episode_id: String,
        pub status: WatchStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub watched_at: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SeriesProgress {
        pub series_percentage: f64,
        pub season_percentage: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct NextEpisode {
        pub episode_id: String,
        pub series_id: String,
        pub season_number: i32,
        pub episode_number: i32,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ProgressResponse {
        pub episode: EpisodeProgress,
        pub progress: SeriesProgress,
        pub next_episode: Option<NextEpisode>,
    }

    #[derive(Debug, Deserialize)]
    pub struct MarkWatchedRequest {
        pub watched: bool,
    }
}

pub mod dashboard {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SeriesRef {
        pub id: String,
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub poster_path: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct EpisodeRef {
        pub id: String,
        pub season_number: i32,
        pub episode_number: i32,
        pub name: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ContinueWatchingItem {
        pub series: SeriesRef,
        pub next_episode: EpisodeRef,
        pub progress: ProgressInfo,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ProgressInfo {
        pub percentage: f64,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct UpcomingItem {
        pub series: SeriesRef,
        pub episode: EpisodeRef,
        pub air_date: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct HistoryItem {
        pub episode: EpisodeRef,
        pub series: SeriesRef,
        pub watched_at: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DashboardResponse {
        pub continue_watching: Vec<ContinueWatchingItem>,
        pub upcoming: Vec<UpcomingItem>,
        pub recent_history: Vec<HistoryItem>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct HistoryResponse {
        pub items: Vec<HistoryItem>,
        pub next_cursor: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CalendarDay {
        pub date: String,
        pub episodes: Vec<CalendarEpisode>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CalendarEpisode {
        pub series: SeriesRef,
        pub episode: EpisodeRef,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CalendarResponse {
        pub items: Vec<CalendarDay>,
    }
}
