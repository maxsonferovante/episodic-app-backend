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
    #[serde(rename = "original_name")]
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
    #[serde(rename = "imdb_id")]
    pub imdb_id: Option<String>,
    #[serde(rename = "watch_providers")]
    pub watch_providers: Option<TmdbWatchProviders>,
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
    pub name: String,
    pub overview: Option<String>,
    #[serde(rename = "poster_path")]
    pub poster_path: Option<String>,
    #[serde(rename = "season_number")]
    pub season_number: i32,
    #[serde(rename = "air_date")]
    pub air_date: Option<String>,
    #[serde(rename = "episode_count")]
    pub episode_count: i32,
}

#[derive(Debug, Deserialize)]
pub struct TmdbSeasonDetail {
    pub id: i64,
    pub name: String,
    pub overview: Option<String>,
    #[serde(rename = "poster_path")]
    pub poster_path: Option<String>,
    #[serde(rename = "season_number")]
    pub season_number: i32,
    #[serde(rename = "air_date")]
    pub air_date: Option<String>,
    pub episodes: Vec<TmdbEpisode>,
}

#[derive(Debug, Deserialize)]
pub struct TmdbEpisode {
    pub id: i64,
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
    let resp: TmdbSearchResponse = client
        .get(format!("{}/search/tv", base_url()))
        .query(&[("api_key", api_key().as_str()), ("query", query), ("page", &page.to_string())])
        .send()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get_tv_details(tmdb_id: i64) -> Result<TmdbTvDetails, reqwest::Error> {
    let client = Client::new();
    let resp: TmdbTvDetails = client
        .get(format!("{}/tv/{}", base_url(), tmdb_id))
        .query(&[("api_key", api_key().as_str()), ("append_to_response", "watch_providers")])
        .send()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get_tv_seasons(tmdb_id: i64) -> Result<Vec<TmdbSeason>, reqwest::Error> {
    let client = Client::new();
    let resp: serde_json::Value = client
        .get(format!("{}/tv/{}", base_url(), tmdb_id))
        .query(&[("api_key", api_key().as_str())])
        .send()
        .await?
        .json()
        .await?;

    let seasons = resp["seasons"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(TmdbSeason {
                        id: s["id"].as_i64()?,
                        name: s["name"].as_str()?.to_string(),
                        overview: s["overview"].as_str().map(|s| s.to_string()),
                        poster_path: s["poster_path"].as_str().map(|s| s.to_string()),
                        season_number: s["season_number"].as_i64()? as i32,
                        air_date: s["air_date"].as_str().map(|s| s.to_string()),
                        episode_count: s["episode_count"].as_i64()? as i32,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(seasons)
}

pub async fn get_tv_season_detail(tmdb_id: i64, season_number: i32) -> Result<TmdbSeasonDetail, reqwest::Error> {
    let client = Client::new();
    let resp: TmdbSeasonDetail = client
        .get(format!("{}/tv/{}/season/{}", base_url(), tmdb_id, season_number))
        .query(&[("api_key", api_key().as_str())])
        .send()
        .await?
        .json()
        .await?;
    Ok(resp)
}
