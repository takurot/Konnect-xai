use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SexpError {
    #[error("Parse error at offset {offset}: {message}")]
    Parse { offset: usize, message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Missing node: {0}")]
    MissingNode(String),

    #[error("Invalid value: {0}")]
    InvalidValue(String),

    /// The document no longer matches the revision the caller edited.
    ///
    /// Callers must reload and reapply their operation instead of overwriting
    /// the newer document.
    #[error("write conflict: {} changed since it was read", crate::paths::display_path(.path))]
    Conflict { path: PathBuf },

    /// Atomic replacement committed, but the immediate readback did not
    /// establish the requested state. Retrying may duplicate completed work.
    #[error("mutation outcome uncertain for {}: {reason}", crate::paths::display_path(.path))]
    MutationOutcomeUncertain { path: PathBuf, reason: String },

    /// KiCad owns, or may still own, the design document through its sibling
    /// lock.
    ///
    /// KiCad lock files identify only a username and hostname, so their
    /// presence cannot be distinguished reliably from a stale lock. Writers
    /// must fail closed and leave both the document and the lock untouched.
    #[error("KiCad editor lock blocks write to {}: {}", crate::paths::display_path(.path), crate::paths::display_path(.lock_path))]
    KiCadEditorLocked {
        path: PathBuf,
        lock_path: PathBuf,
        /// Inspection failures are retained so callers can distinguish an
        /// observed lock from a lock whose absence could not be established.
        inspection_error: Option<String>,
    },

    /// A revision-aware command found that one of its target items no longer
    /// matches the exact item revision on which the command was prepared.
    #[error("item conflict in {}: {item}: {reason}", crate::paths::display_path(.path))]
    ItemConflict {
        path: PathBuf,
        item: String,
        reason: String,
    },

    /// A durable multi-file transaction found content that matches neither
    /// the recorded before image nor the intended replacement.
    #[error("transaction conflict in {}: {}: {reason}", crate::paths::display_path(.journal), crate::paths::display_path(.path))]
    TransactionConflict {
        path: PathBuf,
        journal: PathBuf,
        reason: String,
    },
}
