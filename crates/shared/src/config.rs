//! Runtime configuration shared across the lambda crates.

/// Country used when reading TMDB watch providers. Single source of truth so
/// the catalog reader and the hydrate worker agree on what they store.
pub const DEFAULT_PROVIDER_COUNTRY: &str = "US";

/// Provider country for the current deployment, overridable via `TMDB_COUNTRY`.
pub fn provider_country() -> String {
    std::env::var("TMDB_COUNTRY").unwrap_or_else(|_| DEFAULT_PROVIDER_COUNTRY.to_string())
}
