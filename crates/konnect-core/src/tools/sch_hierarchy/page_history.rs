//! Durable page allocation history shared by all roots of a saved project.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const FILE_NAME: &str = ".konnect-sheet-pages.json";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
struct RootIdentity {
    project: Option<PathBuf>,
    schematic: PathBuf,
    uuid: String,
}

#[derive(Deserialize, Serialize)]
struct Allocation {
    root: RootIdentity,
    last_page: u32,
}

#[derive(Deserialize, Serialize)]
struct History {
    version: u32,
    allocations: Vec<Allocation>,
}

pub(super) struct PageHistory {
    path: PathBuf,
    before: Option<String>,
    active: Vec<RootIdentity>,
    allocations: BTreeMap<RootIdentity, u32>,
}

pub(super) enum PageHistoryInput {
    Write(FileTransition),
    Read(PathBuf, String),
}

impl PageHistory {
    pub fn load(
        directory: &Path,
        project: Option<&Path>,
        roots: Vec<(PathBuf, String)>,
    ) -> anyhow::Result<Self> {
        let path = directory.join(FILE_NAME);
        let before = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "Invalid page history file {}",
                    path.display()
                );
                Some(read_consistent(&path)?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut allocations = BTreeMap::new();
        if let Some(before) = &before {
            let history: History = serde_json::from_str(before).map_err(|error| {
                anyhow::anyhow!("Cannot read page history {}: {error}", path.display())
            })?;
            anyhow::ensure!(
                history.version == VERSION,
                "Unsupported page history version in {}",
                path.display()
            );
            for allocation in history.allocations {
                anyhow::ensure!(
                    allocations
                        .insert(allocation.root, allocation.last_page)
                        .is_none(),
                    "Duplicate root entry in page history {}",
                    path.display()
                );
            }
        }
        let key_path = |path: &Path| path.strip_prefix(directory).unwrap_or(path).to_path_buf();
        let project = project.map(key_path);
        let active = roots
            .into_iter()
            .map(|(schematic, uuid)| RootIdentity {
                project: project.clone(),
                schematic: key_path(&schematic),
                uuid,
            })
            .collect();
        Ok(Self {
            path,
            before,
            active,
            allocations,
        })
    }

    pub fn high_water(&self) -> u32 {
        self.active
            .iter()
            .filter_map(|root| self.allocations.get(root))
            .copied()
            .max()
            .unwrap_or_default()
    }

    pub fn commit_input(mut self, high_water: u32) -> anyhow::Result<PageHistoryInput> {
        let mut changed = false;
        for root in self.active {
            let previous = self.allocations.insert(root, high_water);
            changed |= previous != Some(high_water);
        }
        if !changed && let Some(before) = self.before {
            return Ok(PageHistoryInput::Read(self.path, before));
        }
        let history = History {
            version: VERSION,
            allocations: self
                .allocations
                .into_iter()
                .map(|(root, last_page)| Allocation { root, last_page })
                .collect(),
        };
        let mut after = serde_json::to_string_pretty(&history)?;
        after.push('\n');
        let transition = match self.before {
            Some(before) => FileTransition::replace(self.path, before, after),
            None => FileTransition::create(self.path, after),
        };
        Ok(PageHistoryInput::Write(transition))
    }
}
