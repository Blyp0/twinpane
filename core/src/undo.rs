//! Undo log. Every mutating operation records the reversible [`Action`]s it
//! performed as one [`Transaction`]. Undo walks a transaction backwards.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{io_err, Error, Result};

/// One reversible step. Paths are absolute or as given by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    /// A new item was created by copying. Undo moves it to the holding area.
    Copied { created: PathBuf },
    /// An item was moved. Undo moves it back.
    Moved { from: PathBuf, to: PathBuf },
    /// An item was moved to the holding area (delete or overwrite).
    /// Undo restores it to `original`.
    Trashed { original: PathBuf, held: PathBuf },
    /// A directory was created. Undo removes it if empty, otherwise holds it.
    CreatedDir { path: PathBuf },
    /// An item was renamed. Undo renames it back.
    Renamed { from: PathBuf, to: PathBuf },
    /// Several items were renamed together (safe for swaps and cycles).
    /// Undo reverses them together.
    RenameBatch { pairs: Vec<(PathBuf, PathBuf)> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub id: u64,
    pub description: String,
    pub actions: Vec<Action>,
}

fn default_depth() -> usize {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoLog {
    transactions: Vec<Transaction>,
    next_id: u64,
    #[serde(default = "default_depth")]
    max_depth: usize,
    /// Total transactions pushed in this session (not persisted).
    #[serde(skip)]
    pushed: u64,
}

impl Default for UndoLog {
    fn default() -> Self {
        UndoLog {
            transactions: Vec::new(),
            next_id: 1,
            max_depth: default_depth(),
            pushed: 0,
        }
    }
}

impl UndoLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve an id for a transaction (also names its holding sub-folder).
    pub fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn push(&mut self, txn: Transaction) {
        self.pushed += 1;
        self.transactions.push(txn);
        while self.transactions.len() > self.max_depth {
            self.transactions.remove(0);
        }
    }

    /// Number of transactions pushed so far in this session.
    pub fn pushed_count(&self) -> u64 {
        self.pushed
    }

    pub fn pop(&mut self) -> Option<Transaction> {
        self.transactions.pop()
    }

    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    pub fn clear(&mut self) {
        self.transactions.clear();
    }

    /// Descriptions, oldest first (the last one is what `undo` would revert).
    pub fn descriptions(&self) -> Vec<&str> {
        self.transactions
            .iter()
            .map(|t| t.description.as_str())
            .collect()
    }

    /// Write the log atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Other(format!("could not serialise undo log: {e}")))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| io_err(&tmp, e))?;
        fs::rename(&tmp, path).map_err(|e| io_err(path, e))?;
        Ok(())
    }

    /// Load a log; a missing file yields an empty log.
    pub fn load(path: &Path) -> Result<UndoLog> {
        if !path.exists() {
            return Ok(UndoLog::default());
        }
        let text = fs::read_to_string(path).map_err(|e| io_err(path, e))?;
        let mut log: UndoLog = serde_json::from_str(&text)
            .map_err(|e| Error::Other(format!("corrupt undo log {}: {e}", path.display())))?;
        let max_id = log.transactions.iter().map(|t| t.id).max().unwrap_or(0);
        log.next_id = log.next_id.max(max_id + 1);
        Ok(log)
    }
}
