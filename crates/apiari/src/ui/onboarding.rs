//! Onboarding utilities — checks whether the user has been onboarded
//! and writes the `.onboarded` marker when done.
//!
//! The actual onboarding UX lives inside the normal dashboard (see `app::OnboardingState`).
//! Panels progressively brighten as Bee narrates them into existence.

use crate::config;

/// Path to the onboarded marker file.
fn onboarded_marker_path() -> std::path::PathBuf {
    config::config_dir().join(".onboarded")
}

/// Returns true if the user needs onboarding (marker file absent).
pub fn needs_onboarding() -> bool {
    !onboarded_marker_path().exists()
}

/// Write the `.onboarded` marker so onboarding doesn't run again.
pub fn mark_onboarded() {
    let path = onboarded_marker_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(path, "done\n");
}
