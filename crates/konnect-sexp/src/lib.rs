pub mod board;
pub mod bounds;
pub mod command;
pub mod error;
pub mod geometry;
pub mod layers;
pub mod net;
pub mod parser;
pub mod schematic;
pub mod transaction;
pub mod writer;

pub use command::{
    commit_command, prepare_command, DocumentRevision, ItemAnchor, ItemChange, ItemId,
    SchematicCommand, TransactionOutcome,
};
pub use error::SexpError;
pub use geometry::{transform_direction, transform_pin, PinTransform};
pub use parser::{parse_sexp, SexpNode};
pub use transaction::{
    abandon_file_transaction, commit_file_transaction, inspect_file_transactions,
    recover_file_transaction, recover_file_transactions, AbandonedTransaction, FileTransition,
    RecoveryOutcome, TransactionCommit, TransactionStatus, TransactionTargetState,
    TransactionTargetStatus,
};
pub use writer::{
    apply_edits, read_consistent, transact_atomic, write_atomic, write_atomic_if_unchanged,
    write_new_atomic, SexpEdit,
};

/// Escape the contents of a KiCad quoted string.
pub fn escaped_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Encode a complete KiCad quoted string, preserving quotes and backslashes.
pub fn quoted(value: &str) -> String {
    format!("\"{}\"", escaped_string(value))
}

#[cfg(test)]
mod string_tests {
    #[test]
    fn quoted_strings_roundtrip() {
        for value in ["", "plain", "a\"b\\c", "0.1\" pitch", "日本語\\\""] {
            let tree = crate::parse_sexp(&format!("(value {})", crate::quoted(value))).unwrap();
            assert_eq!(tree.get(1).and_then(|n| n.as_str()), Some(value));
        }
    }
}
