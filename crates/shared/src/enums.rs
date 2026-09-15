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

/// Library entry status values.
pub mod library_status {
    pub const IN_PROGRESS: &str = "IN_PROGRESS";
    pub const CAUGHT_UP: &str = "CAUGHT_UP";
    pub const COMPLETED: &str = "COMPLETED";
}
