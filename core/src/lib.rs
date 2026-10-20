//! # twinpane-core
//!
//! The file engine behind Twinpane. It has no UI and no global state, so it can
//! be tested headlessly and driven by any front end.
//!
//! Safety model: nothing is permanently deleted by normal operations; items go
//! to a holding area and every change is recorded in an undo log.

pub mod api;
pub mod error;
pub mod listing;
pub mod metadata;
pub mod ops;
pub mod rename;
pub mod search;
pub mod sync;
pub mod undo;

pub use error::{Error, Result};
pub use listing::{list_dir, natural_cmp, sort_entries, Entry, EntryKind, ListOptions, SortKey, SortOrder};
pub use metadata::{
    has_exif_extension, list_rows, read_meta, sort_rows, DateTime, FileMeta, Row, RowSort,
};
pub use ops::{validate_name, ConflictPolicy, Engine, OpReport};
pub use rename::{
    apply_plan, build_plan, CaseMode, ItemStatus, PlanItem, Position, RenameOptions, RenamePlan,
    RenameRule, Target,
};
pub use search::{
    search, KindFilter, NameMode, SearchHit, SearchOutcome, SearchQuery,
};
pub use sync::{
    apply_sync, compare_dirs, plan_sync, CompareMethod, CompareOptions, DiffItem, DiffStatus,
    SyncAction, SyncKind, SyncMode, SyncPlan,
};
pub use undo::{Action, Transaction, UndoLog};
