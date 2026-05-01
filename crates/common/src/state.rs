//! Generic JSON state persistence (load/save).
//!
//! Provides [`load_state`] and [`save_state`] for any type that implements
//! serde's `Serialize` / `DeserializeOwned`. State files are written atomically
//! (write to a temp file, then rename) so a crash mid-write never corrupts the
//! on-disk state.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io;
use std::path::Path;

/// Load state from a JSON file.
///
/// - If the file does not exist, returns the type's `Default` value.
/// - If the file exists but cannot be parsed, returns an error.
///
/// # Errors
///
/// Returns `io::Error` if the file exists but cannot be read or parsed.
pub fn load_state<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match std::fs::read_to_string(path) {
        Ok(data) => {
            serde_json::from_str(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

/// Save state to a JSON file atomically.
///
/// Writes to a temporary file in the same directory, then renames it into
/// place. This guarantees that the state file is always either the old
/// version or the new version, never a partially-written mix.
///
/// Parent directories are created automatically if they don't exist.
///
/// # Errors
///
/// Returns `io::Error` if serialization, directory creation, writing,
/// or renaming fails.
pub fn save_state<T: Serialize>(path: &Path, state: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let data = serde_json::to_string_pretty(state).map_err(io::Error::other)?;

    // Write to a sibling temp file, then atomically rename.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &data)?;
    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::fs;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
    struct TestState {
        counter: u64,
        name: String,
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("apiari-state-test-save-load");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let state = TestState {
            counter: 42,
            name: "test".into(),
        };

        save_state(&path, &state).unwrap();
        let loaded: TestState = load_state(&path).unwrap();
        assert_eq!(loaded, state);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_missing_returns_default() {
        let path = std::env::temp_dir().join("apiari-state-test-missing-file.json");
        let _ = fs::remove_file(&path);

        let loaded: TestState = load_state(&path).unwrap();
        assert_eq!(loaded, TestState::default());
    }

    #[test]
    fn test_save_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("apiari-state-test-parents/a/b/c");
        let _ = fs::remove_dir_all(std::env::temp_dir().join("apiari-state-test-parents"));
        let path = dir.join("state.json");

        let state = TestState {
            counter: 1,
            name: "nested".into(),
        };

        save_state(&path, &state).unwrap();
        assert!(path.exists());

        let loaded: TestState = load_state(&path).unwrap();
        assert_eq!(loaded, state);

        let _ = fs::remove_dir_all(std::env::temp_dir().join("apiari-state-test-parents"));
    }

    #[test]
    fn test_atomic_write_no_temp_file_left() {
        let dir = std::env::temp_dir().join("apiari-state-test-atomic");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let tmp_path = dir.join("state.json.tmp");

        let state = TestState {
            counter: 99,
            name: "atomic".into(),
        };

        save_state(&path, &state).unwrap();

        // The temp file should have been renamed away
        assert!(path.exists());
        assert!(!tmp_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_corrupt_file_returns_error() {
        let dir = std::env::temp_dir().join("apiari-state-test-corrupt");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        fs::write(&path, "not valid json!!!").unwrap();

        let result: io::Result<TestState> = load_state(&path);
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_overwrite_existing() {
        let dir = std::env::temp_dir().join("apiari-state-test-overwrite");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let state1 = TestState {
            counter: 1,
            name: "first".into(),
        };
        save_state(&path, &state1).unwrap();

        let state2 = TestState {
            counter: 2,
            name: "second".into(),
        };
        save_state(&path, &state2).unwrap();

        let loaded: TestState = load_state(&path).unwrap();
        assert_eq!(loaded, state2);

        let _ = fs::remove_dir_all(&dir);
    }
}
