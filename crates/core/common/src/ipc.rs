//! Generic JSONL read/write with cursor-based polling.
//!
//! Provides [`JsonlReader`] and [`JsonlWriter`] for line-delimited JSON files.
//! The reader tracks a byte offset so that each call to [`JsonlReader::poll`]
//! only returns newly appended records since the last read.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

/// Reads JSONL records from a file, tracking the byte offset so that
/// each poll only returns lines appended since the previous read.
///
/// Generic over any `T: DeserializeOwned`.
#[derive(Debug)]
pub struct JsonlReader<T> {
    path: PathBuf,
    offset: u64,
    _marker: PhantomData<T>,
}

impl<T: DeserializeOwned> JsonlReader<T> {
    /// Create a new reader for the given path, starting at byte offset 0.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            _marker: PhantomData,
        }
    }

    /// Create a new reader starting at the given byte offset.
    ///
    /// Useful when restoring from persisted state — you can resume reading
    /// from where you left off without replaying old messages.
    pub fn with_offset(path: impl Into<PathBuf>, offset: u64) -> Self {
        Self {
            path: path.into(),
            offset,
            _marker: PhantomData,
        }
    }

    /// Return the current byte offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Set the byte offset (e.g. when restoring from persisted state).
    pub fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }

    /// Skip to the end of the file so that subsequent polls only see new data.
    ///
    /// Returns the new offset, or 0 if the file does not exist.
    pub fn skip_to_end(&mut self) -> io::Result<u64> {
        match fs::metadata(&self.path) {
            Ok(meta) => {
                self.offset = meta.len();
                Ok(self.offset)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.offset = 0;
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }

    /// Read any new lines appended since the last poll.
    ///
    /// Returns a vector of successfully deserialized records. Malformed lines
    /// are silently skipped (the offset still advances past them).
    pub fn poll(&mut self) -> io::Result<Vec<T>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&self.path)?;
        let file_len = file.metadata()?.len();

        if file_len <= self.offset {
            return Ok(Vec::new());
        }

        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(self.offset))?;

        let mut records = Vec::new();
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line)?;
            if bytes_read == 0 {
                break;
            }
            self.offset += bytes_read as u64;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Ok(record) = serde_json::from_str::<T>(trimmed) {
                records.push(record);
            }
            // Malformed lines are silently skipped.
        }

        Ok(records)
    }
}

/// Appends JSONL records to a file, creating parent directories as needed.
///
/// Generic over any `T: Serialize`.
#[derive(Debug)]
pub struct JsonlWriter<T> {
    path: PathBuf,
    _marker: PhantomData<T>,
}

impl<T: Serialize> JsonlWriter<T> {
    /// Create a new writer for the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            _marker: PhantomData,
        }
    }

    /// Return the file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a single record as a JSON line.
    ///
    /// Creates parent directories and the file itself if they don't exist.
    pub fn append(&self, record: &T) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        let json = serde_json::to_string(record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{}", json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestMsg {
        id: u32,
        text: String,
    }

    #[test]
    fn test_write_and_read() {
        let dir = std::env::temp_dir().join("apiari-ipc-test-write-read");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl");

        let writer = JsonlWriter::<TestMsg>::new(&path);
        let mut reader = JsonlReader::<TestMsg>::new(&path);

        // Write two records
        writer
            .append(&TestMsg {
                id: 1,
                text: "hello".into(),
            })
            .unwrap();
        writer
            .append(&TestMsg {
                id: 2,
                text: "world".into(),
            })
            .unwrap();

        // Poll should return both
        let records = reader.poll().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].id, 1);
        assert_eq!(records[1].id, 2);

        // Poll again with no new data
        let records = reader.poll().unwrap();
        assert!(records.is_empty());

        // Write a third record
        writer
            .append(&TestMsg {
                id: 3,
                text: "!".into(),
            })
            .unwrap();

        // Poll should return only the new record
        let records = reader.poll().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_reader_nonexistent_file() {
        let path = std::env::temp_dir().join("apiari-ipc-test-nonexistent.jsonl");
        let _ = fs::remove_file(&path);

        let mut reader = JsonlReader::<TestMsg>::new(&path);
        let records = reader.poll().unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_skip_to_end() {
        let dir = std::env::temp_dir().join("apiari-ipc-test-skip");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl");

        let writer = JsonlWriter::<TestMsg>::new(&path);
        writer
            .append(&TestMsg {
                id: 1,
                text: "old".into(),
            })
            .unwrap();

        let mut reader = JsonlReader::<TestMsg>::new(&path);
        reader.skip_to_end().unwrap();

        // Should not see the old record
        let records = reader.poll().unwrap();
        assert!(records.is_empty());

        // New record should be visible
        writer
            .append(&TestMsg {
                id: 2,
                text: "new".into(),
            })
            .unwrap();
        let records = reader.poll().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_malformed_lines_skipped() {
        let dir = std::env::temp_dir().join("apiari-ipc-test-malformed");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl");

        // Write a valid record, a malformed line, and another valid record
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, r#"{{"id":1,"text":"good"}}"#).unwrap();
        writeln!(file, "not valid json").unwrap();
        writeln!(file, r#"{{"id":2,"text":"also good"}}"#).unwrap();

        let mut reader = JsonlReader::<TestMsg>::new(&path);
        let records = reader.poll().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].id, 1);
        assert_eq!(records[1].id, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_with_offset() {
        let dir = std::env::temp_dir().join("apiari-ipc-test-with-offset");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl");

        let writer = JsonlWriter::<TestMsg>::new(&path);
        writer
            .append(&TestMsg {
                id: 1,
                text: "first".into(),
            })
            .unwrap();

        // Read first to get the offset
        let mut reader = JsonlReader::<TestMsg>::new(&path);
        let _ = reader.poll().unwrap();
        let saved_offset = reader.offset();

        // Write another record
        writer
            .append(&TestMsg {
                id: 2,
                text: "second".into(),
            })
            .unwrap();

        // Create a new reader from the saved offset
        let mut reader2 = JsonlReader::<TestMsg>::with_offset(&path, saved_offset);
        let records = reader2.poll().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
