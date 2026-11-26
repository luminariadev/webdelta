use std::fs;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const LATEST_TABLE: TableDefinition<&str, &str> = TableDefinition::new("latest");
const HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("history");

/// Stored snapshot for a single watch evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotRecord {
    pub watch_id: String,
    pub checked_at: DateTime<Utc>,
    pub status_code: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: String,
    pub extracted_text: String,
    pub numeric_value: Option<f64>,
    pub elapsed_ms: u64,
}

/// Embedded ACID state store backed by `redb`.
#[derive(Debug, Clone)]
pub struct StateStore {
    db: Arc<Database>,
}

impl StateStore {
    /// Opens or creates a `redb` database at `path` and ensures tables exist.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let db = Database::create(path).map_err(|err| {
            Error::Storage(format!(
                "failed to open state database '{}': {err}",
                path.display()
            ))
        })?;

        let write_txn = db
            .begin_write()
            .map_err(|err| Error::Storage(format!("failed to begin init transaction: {err}")))?;
        {
            write_txn
                .open_table(LATEST_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open latest table: {err}")))?;
            write_txn
                .open_table(HISTORY_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open history table: {err}")))?;
        }
        write_txn
            .commit()
            .map_err(|err| Error::Storage(format!("failed to commit init transaction: {err}")))?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Writes a new snapshot to both `latest` and `history`, pruning entries beyond `max_history`.
    pub fn record_snapshot(&self, record: &SnapshotRecord, max_history: usize) -> Result<()> {
        let encoded = serde_json::to_string(record)
            .map_err(|err| Error::Storage(format!("failed to serialize snapshot: {err}")))?;

        let write_txn = self
            .db
            .begin_write()
            .map_err(|err| Error::Storage(format!("failed to begin write transaction: {err}")))?;
        {
            let mut latest = write_txn
                .open_table(LATEST_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open latest table: {err}")))?;
            latest
                .insert(record.watch_id.as_str(), encoded.as_str())
                .map_err(|err| {
                    Error::Storage(format!("failed to insert latest snapshot: {err}"))
                })?;

            let mut history = write_txn
                .open_table(HISTORY_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open history table: {err}")))?;

            let ts_millis = record.checked_at.timestamp_millis().max(0) as u128;
            let ts_prefix = format!("{}:{ts_millis:020}:", record.watch_id);
            let mut next_seq: u64 = 0;

            {
                let range = history
                    .range(ts_prefix.as_str()..)
                    .map_err(|err| Error::Storage(format!("failed to scan history keys: {err}")))?;
                for item in range {
                    let (key, _) =
                        item.map_err(|err| Error::Storage(format!("history read error: {err}")))?;
                    let key_str = key.value();
                    if !key_str.starts_with(&ts_prefix) {
                        break;
                    }
                    if let Some(seq_part) = key_str.strip_prefix(&ts_prefix) {
                        if let Ok(parsed) = seq_part.parse::<u64>() {
                            next_seq = next_seq.max(parsed.saturating_add(1));
                        }
                    }
                }
            }

            let history_key = format!("{ts_prefix}{next_seq:06}");
            history
                .insert(history_key.as_str(), encoded.as_str())
                .map_err(|err| Error::Storage(format!("failed to insert history entry: {err}")))?;

            if max_history > 0 {
                let watch_prefix = format!("{}:", record.watch_id);
                let mut keys = Vec::new();
                {
                    let range = history.range(watch_prefix.as_str()..).map_err(|err| {
                        Error::Storage(format!("failed to scan watch history: {err}"))
                    })?;
                    for item in range {
                        let (key, _) = item
                            .map_err(|err| Error::Storage(format!("history read error: {err}")))?;
                        let key_str = key.value();
                        if !key_str.starts_with(&watch_prefix) {
                            break;
                        }
                        keys.push(key_str.to_string());
                    }
                }

                if keys.len() > max_history {
                    let excess = keys.len() - max_history;
                    for stale_key in keys.into_iter().take(excess) {
                        history.remove(stale_key.as_str()).map_err(|err| {
                            Error::Storage(format!("failed to prune stale history: {err}"))
                        })?;
                    }
                }
            }
        }
        write_txn
            .commit()
            .map_err(|err| Error::Storage(format!("failed to commit snapshot: {err}")))?;

        Ok(())
    }

    /// Updates check metadata on `latest` without adding a duplicate history entry.
    pub fn update_latest_metadata(
        &self,
        watch_id: &str,
        checked_at: DateTime<Utc>,
        status_code: u16,
        elapsed_ms: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Result<()> {
        let Some(mut existing) = self.get_latest(watch_id)? else {
            return Ok(());
        };
        existing.checked_at = checked_at;
        existing.elapsed_ms = elapsed_ms;
        existing.status_code = status_code;
        if etag.is_some() {
            existing.etag = etag;
        }
        if last_modified.is_some() {
            existing.last_modified = last_modified;
        }

        let encoded = serde_json::to_string(&existing)
            .map_err(|err| Error::Storage(format!("failed to serialize snapshot: {err}")))?;
        let write_txn = self.db.begin_write().map_err(|err| {
            Error::Storage(format!("failed to begin metadata transaction: {err}"))
        })?;
        {
            let mut latest = write_txn
                .open_table(LATEST_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open latest table: {err}")))?;
            latest.insert(watch_id, encoded.as_str()).map_err(|err| {
                Error::Storage(format!("failed to update latest snapshot: {err}"))
            })?;
        }
        write_txn.commit().map_err(|err| {
            Error::Storage(format!("failed to commit metadata transaction: {err}"))
        })?;

        Ok(())
    }

    /// Returns the most recent snapshot for `watch_id`, if any.
    pub fn get_latest(&self, watch_id: &str) -> Result<Option<SnapshotRecord>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|err| Error::Storage(format!("failed to begin read transaction: {err}")))?;
        let latest = read_txn
            .open_table(LATEST_TABLE)
            .map_err(|err| Error::Storage(format!("failed to open latest table: {err}")))?;

        let Some(guard) = latest
            .get(watch_id)
            .map_err(|err| Error::Storage(format!("failed to read latest snapshot: {err}")))?
        else {
            return Ok(None);
        };

        let parsed: SnapshotRecord = serde_json::from_str(guard.value()).map_err(|err| {
            Error::Storage(format!("failed to decode latest snapshot JSON: {err}"))
        })?;
        Ok(Some(parsed))
    }

    /// Returns up to `limit` snapshots for `watch_id`, ordered newest first.
    pub fn get_history(&self, watch_id: &str, limit: usize) -> Result<Vec<SnapshotRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read_txn = self
            .db
            .begin_read()
            .map_err(|err| Error::Storage(format!("failed to begin read transaction: {err}")))?;
        let history = read_txn
            .open_table(HISTORY_TABLE)
            .map_err(|err| Error::Storage(format!("failed to open history table: {err}")))?;

        let prefix = format!("{watch_id}:");
        let mut records = Vec::new();
        let range = history
            .range(prefix.as_str()..)
            .map_err(|err| Error::Storage(format!("failed to scan history table: {err}")))?;

        for item in range {
            let (key, val) =
                item.map_err(|err| Error::Storage(format!("failed reading history row: {err}")))?;
            if !key.value().starts_with(&prefix) {
                break;
            }
            let record: SnapshotRecord = serde_json::from_str(val.value())
                .map_err(|err| Error::Storage(format!("failed to decode history JSON: {err}")))?;
            records.push(record);
        }

        records.reverse();
        records.truncate(limit);
        Ok(records)
    }

    /// Returns `(previous, current)` snapshots for diffing, if at least two exist.
    pub fn get_latest_pair(
        &self,
        watch_id: &str,
    ) -> Result<Option<(SnapshotRecord, SnapshotRecord)>> {
        let mut items = self.get_history(watch_id, 2)?;
        if items.len() < 2 {
            return Ok(None);
        }
        let current = items.remove(0);
        let previous = items.remove(0);
        Ok(Some((previous, current)))
    }

    /// Removes all latest and historical snapshots belonging to `watch_id`.
    pub fn purge_watch(&self, watch_id: &str) -> Result<usize> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|err| Error::Storage(format!("failed to begin purge transaction: {err}")))?;
        let mut removed = 0usize;
        {
            let mut latest = write_txn
                .open_table(LATEST_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open latest table: {err}")))?;
            if latest
                .remove(watch_id)
                .map_err(|err| Error::Storage(format!("failed to remove latest entry: {err}")))?
                .is_some()
            {
                removed += 1;
            }

            let mut history = write_txn
                .open_table(HISTORY_TABLE)
                .map_err(|err| Error::Storage(format!("failed to open history table: {err}")))?;
            let prefix = format!("{watch_id}:");
            let mut keys = Vec::new();
            {
                let range = history.range(prefix.as_str()..).map_err(|err| {
                    Error::Storage(format!("failed to scan history for purge: {err}"))
                })?;
                for item in range {
                    let (key, _) =
                        item.map_err(|err| Error::Storage(format!("history read error: {err}")))?;
                    let key_str = key.value();
                    if !key_str.starts_with(&prefix) {
                        break;
                    }
                    keys.push(key_str.to_string());
                }
            }

            for key in keys {
                if history
                    .remove(key.as_str())
                    .map_err(|err| Error::Storage(format!("failed to remove history key: {err}")))?
                    .is_some()
                {
                    removed += 1;
                }
            }
        }
        write_txn
            .commit()
            .map_err(|err| Error::Storage(format!("failed to commit purge transaction: {err}")))?;

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use tempfile::tempdir;

    fn sample_record(id: &str, text: &str, offset_ms: i64) -> SnapshotRecord {
        SnapshotRecord {
            watch_id: id.to_string(),
            checked_at: DateTime::from_timestamp_millis(1_700_000_000_000 + offset_ms).unwrap(),
            status_code: 200,
            etag: Some("etag-1".to_string()),
            last_modified: None,
            content_hash: format!("hash-{text}"),
            extracted_text: text.to_string(),
            numeric_value: None,
            elapsed_ms: 12,
        }
    }

    #[test]
    fn test_store_history_pruning_pair_and_purge() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nested").join("state.redb");
        let store = StateStore::open(&db_path).unwrap();

        for i in 0..5 {
            store
                .record_snapshot(&sample_record("w1", &format!("v{i}"), i * 10), 3)
                .unwrap();
        }
        store
            .record_snapshot(&sample_record("w2", "other", 100), 3)
            .unwrap();

        let hist = store.get_history("w1", 10).unwrap();
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].extracted_text, "v4");
        assert_eq!(hist[1].extracted_text, "v3");
        assert_eq!(hist[2].extracted_text, "v2");

        let (prev, curr) = store.get_latest_pair("w1").unwrap().unwrap();
        assert_eq!(prev.extracted_text, "v3");
        assert_eq!(curr.extracted_text, "v4");

        let touched_time = Utc::now() + ChronoDuration::seconds(1);
        store
            .update_latest_metadata("w1", touched_time, 304, 5, None, None)
            .unwrap();
        let latest = store.get_latest("w1").unwrap().unwrap();
        assert_eq!(latest.status_code, 304);
        assert_eq!(latest.extracted_text, "v4");

        let removed = store.purge_watch("w1").unwrap();
        assert_eq!(removed, 4);
        assert!(store.get_latest("w1").unwrap().is_none());
        assert!(store.get_latest("w2").unwrap().is_some());
    }
}
