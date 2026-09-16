//! TMDB API client and response shapes, shared by the catalog lambda and the
//! hydrate worker so both read the same fields from the same endpoints.

use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TmdbSearchResult {
    pub id: i64,
    pub name: String,
    #[serde(rename = "original_name")]
    pub original_name: String,
    #[serde(rename = "first_air_date")]
    pub first_air_date: Option<String>,
    #[serde(rename = "poster_path")]
    pub poster_path: Option<String>,
    pub overview: Option<String>,
    pub vote_average: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbSearchResponse {
    pub results: Vec<TmdbSearchResult>,
    #[serde(rename = "total_pages")]
    pub total_pages: i32,
}

#[derive(Debug, Deserialize)]
pub struct TmdbTvDetails {
    pub id: i64,
    pub name: String,
    #[serde(rename = "original_name", default)]
    pub original_name: String,
    #[serde(rename = "first_air_date")]
    pub first_air_date: Option<String>,
    #[serde(rename = "last_air_date")]
    pub last_air_date: Option<String>,
    #[serde(rename = "poster_path")]
    pub poster_path: Option<String>,
    #[serde(rename = "backdrop_path")]
    pub backdrop_path: Option<String>,
    pub overview: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "number_of_seasons")]
    pub number_of_seasons: Option<i32>,
    #[serde(rename = "number_of_episodes")]
    pub number_of_episodes: Option<i32>,
    /// Present only when `external_ids` is appended to the request.
    #[serde(rename = "imdb_id")]
    pub imdb_id: Option<String>,
    #[serde(rename = "watch_providers")]
    pub watch_providers: Option<TmdbWatchProviders>,
    /// `/tv/{id}` already returns the full season list — no extra call needed.
    #[serde(default)]
    pub seasons: Vec<TmdbSeason>,
    #[serde(rename = "next_episode_to_air")]
    pub next_episode_to_air: Option<TmdbNextEpisode>,
    #[serde(rename = "external_ids")]
    pub external_ids: Option<TmdbExternalIds>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbNextEpisode {
    #[serde(rename = "air_date")]
    pub air_date: Option<String>,
    #[serde(rename = "season_number")]
    pub season_number: Option<i32>,
    #[serde(rename = "episode_number")]
    pub episode_number: Option<i32>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbExternalIds {
    #[serde(rename = "imdb_id")]
    pub imdb_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbWatchProviders {
    pub results: Option<std::collections::HashMap<String, TmdbCountryProviders>>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbCountryProviders {
    pub flatrate: Option<Vec<TmdbProvider>>,
    pub rent: Option<Vec<TmdbProvider>>,
    pub buy: Option<Vec<TmdbProvider>>,
    pub free: Option<Vec<TmdbProvider>>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbProvider {
    #[serde(rename = "provider_id")]
    pub provider_id: i64,
    #[serde(rename = "provider_name")]
    pub provider_name: String,
    #[serde(rename = "logo_path")]
    pub logo_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbSeason {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    pub overview: Option<String>,
    #[serde(rename = "poster_path")]
    pub poster_path: Option<String>,
    #[serde(rename = "season_number")]
    pub season_number: i32,
    #[serde(rename = "air_date")]
    pub air_date: Option<String>,
    #[serde(default, rename = "episode_count")]
    pub episode_count: i32,
}

#[derive(Debug, Deserialize)]
pub struct TmdbSeasonDetail {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    pub overview: Option<String>,
    #[serde(rename = "poster_path")]
    pub poster_path: Option<String>,
    #[serde(rename = "season_number")]
    pub season_number: i32,
    #[serde(rename = "air_date")]
    pub air_date: Option<String>,
    #[serde(default)]
    pub episodes: Vec<TmdbEpisode>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbEpisode {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    pub overview: Option<String>,
    #[serde(rename = "still_path")]
    pub still_path: Option<String>,
    #[serde(rename = "episode_number")]
    pub episode_number: i32,
    #[serde(rename = "air_date")]
    pub air_date: Option<String>,
    pub runtime: Option<i32>,
    #[serde(rename = "vote_average")]
    pub vote_average: Option<f64>,
}

fn api_key() -> String {
    std::env::var("TMDB_API_KEY").unwrap_or_default()
}

fn base_url() -> String {
    std::env::var("TMDB_BASE_URL").unwrap_or_else(|_| "https://api.themoviedb.org/3".to_string())
}

pub async fn search_tv(query: &str, page: i32) -> Result<TmdbSearchResponse, reqwest::Error> {
    let client = Client::new();
    client
        .get(format!("{}/search/tv", base_url()))
        .query(&[("api_key", api_key().as_str()), ("query", query), ("page", &page.to_string())])
        .send()
        .await?
        .json()
        .await
}

/// Series details with providers and external ids appended. Also carries the
/// season list, so callers never need a second `/tv/{id}` request.
pub async fn get_tv_details(tmdb_id: i64) -> Result<TmdbTvDetails, reqwest::Error> {
    let client = Client::new();
    client
        .get(format!("{}/tv/{}", base_url(), tmdb_id))
        .query(&[("api_key", api_key().as_str()), ("append_to_response", "watch_providers,external_ids")])
        .send()
        .await?
        .json()
        .await
}

pub async fn get_tv_season_detail(
    tmdb_id: i64,
    season_number: i32,
) -> Result<TmdbSeasonDetail, reqwest::Error> {
    let client = Client::new();
    client
        .get(format!("{}/tv/{}/season/{}", base_url(), tmdb_id, season_number))
        .query(&[("api_key", api_key().as_str())])
        .send()
        .await?
        .json()
        .await
}

/// Season list for a series. `/tv/{id}` already embeds it, so this is a thin
/// wrapper over the details endpoint.
pub async fn get_tv_seasons(tmdb_id: i64) -> Result<Vec<TmdbSeason>, reqwest::Error> {
    Ok(get_tv_details(tmdb_id).await?.seasons)
}
