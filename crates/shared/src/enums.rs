//! Canonical status/type strings persisted in DynamoDB and returned over the
//! API. Keep every literal here so the backend never compares raw strings.

/// `WatchStatus` values as stored in DynamoDB.
pub mod watch_status {
    pub const WATCHED: &str = "WATCHED";
    pub const UNWATCHED: &str = "UNWATCHED";
    pub const UPCOMING: &str = "UPCOMING";
}

/// `eventType` values for watch events.
pub mod watch_event_type {
    pub const MARK_WATCHED: &str = "MARK_WATCHED";
    pub const UNMARK_WATCHED: &str = "UNMARK_WATCHED";
}

/// TMDB `status` values, as returned by `/tv/{id}`.
pub mod series_status {
    pub const RETURNING: &str = "Returning Series";
    pub const PLANNED: &str = "Planned";
    pub const IN_PRODUCTION: &str = "In Production";
    pub const ENDED: &str = "Ended";
    pub const CANCELED: &str = "Canceled";
    pub const PILOT: &str = "Pilot";

    /// A finished series never changes again, so its cached metadata can be
    /// frozen indefinitely and never re-fetched.
    pub fn is_finished(value: &str) -> bool {
        matches!(value, ENDED | CANCELED)
    }
}

/// `hydrationStatus` values tracked on the canonical series meta row.
pub mod hydration_status {
    pub const PENDING: &str = "PENDING";
    pub const HYDRATING: &str = "HYDRATING";
    pub const COMPLETE: &str = "COMPLETE";
}
