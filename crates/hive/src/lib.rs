//! Hive library — re-exports modules for integration tests.

pub mod buzz;
pub mod presence;
pub mod quest;
pub mod routing;
pub mod signal;
pub mod workspace;

// Re-export ui::inbox for integration tests (daemon writes events here).
pub mod ui {
    pub mod history;
    pub mod inbox;
}
