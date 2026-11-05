//! Canonical URLs for this repository and the upstream it was forked from.
//!
//! Lives in `konnect-ipc` because it is the lowest crate that both the IPC
//! client's error text and `konnect-core` can depend on.

pub const REPO_URL: &str = "https://github.com/takurot/Konnect-xai";

/// The archived project this repository was forked from. Its issue numbers
/// still appear in legacy reliability records.
pub const UPSTREAM_REPO_URL: &str = "https://github.com/mixelpixx/Konnect";

/// A browsable link to `docs/<path>` on this repository's default branch.
pub fn docs_url(path: &str) -> String {
    format!("{REPO_URL}/blob/main/docs/{path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    rust_sources(&path, out);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn docs_url_points_at_this_repository() {
        assert_eq!(
            docs_url("TROUBLESHOOTING.md"),
            "https://github.com/takurot/Konnect-xai/blob/main/docs/TROUBLESHOOTING.md"
        );
    }

    #[test]
    fn no_rust_source_links_to_upstream_docs() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut sources = Vec::new();
        rust_sources(&crates, &mut sources);
        assert!(sources.len() > 50, "scan found too few files: {crates:?}");

        let needle = format!("{}/blob", UPSTREAM_REPO_URL.trim_start_matches("https://"));
        let offenders: Vec<_> = sources
            .iter()
            .filter(|path| std::fs::read_to_string(path).unwrap().contains(&needle))
            .collect();
        assert!(
            offenders.is_empty(),
            "use project_links::docs_url instead of upstream doc links: {offenders:?}"
        );
    }
}
