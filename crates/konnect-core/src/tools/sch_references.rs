//! Project-wide numbered reference validation for component mutations.

use super::sch_annotate::{format_designator, one_package, parse_designator, placed_symbols};
use super::sch_hierarchy::instances::reference_sources;
use super::SheetInstanceContext;
use crate::mcp::{
    error::{ReferencePlacement, ToolErrorKind},
    protocol::CallToolResult,
};
use konnect_sexp::{
    parse_sexp, schematic::write_schematic_if_unchanged,
    transaction::commit_file_transaction_with_dependencies, FileTransition, SexpError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

type Reference = (String, u32);
type Groups = BTreeMap<Reference, Vec<Placement>>;

#[derive(Clone, Debug, PartialEq)]
struct Placement {
    document: PathBuf,
    instance: String,
    reference: String,
    symbol: super::sch_annotate::PlacedSymbol,
}

pub(super) struct ReferenceInventory {
    path: PathBuf,
    directory: PathBuf,
    project: String,
    paths: BTreeSet<String>,
    own: Groups,
    others: Groups,
    dependencies: Vec<(PathBuf, String)>,
}

pub(super) fn is_numbered(reference: &str) -> bool {
    parse_designator(reference).number.is_some()
}

impl ReferenceInventory {
    pub(super) fn load(
        path: &Path,
        before: &str,
        topology_source: &str,
        context: Option<&SheetInstanceContext>,
    ) -> Result<Self, CallToolResult> {
        let stale = |error: anyhow::Error| {
            let reason = format!("Could not verify project references: {error}");
            CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: path.display().to_string(),
                    reason: reason.clone(),
                },
                reason,
            )
        };
        Self::read(path, before, topology_source, context).map_err(stale)
    }

    fn read(
        path: &Path,
        before: &str,
        topology_source: &str,
        context: Option<&SheetInstanceContext>,
    ) -> anyhow::Result<Self> {
        let path = path.canonicalize()?;
        let mut sources = reference_sources(&path, before, topology_source)?;
        let (source, paths) = sources
            .documents
            .remove(&path)
            .ok_or_else(|| anyhow::anyhow!("Missing target schematic {}", path.display()))?;
        if let Some(context) = context {
            anyhow::ensure!(
                context.project_name == sources.project_name
                    && context.instance_paths.iter().eq(paths.iter()),
                "The sheet's project or hierarchy instances changed during planning"
            );
        }
        let journal_anchor = sources.project.as_ref().map_or(&path, |(file, _)| file);
        let directory = journal_anchor
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Missing journal directory for {}", path.display()))?
            .to_path_buf();
        let own = collect(&path, &source, &sources.project_name, &paths)?;
        let mut others = Groups::new();
        let mut dependencies = Vec::new();
        dependencies.extend(sources.project);
        for (document, (source, paths)) in sources.documents {
            for (key, placements) in collect(&document, &source, &sources.project_name, &paths)? {
                others.entry(key).or_default().extend(placements);
            }
            dependencies.push((document, source));
        }
        Ok(Self {
            path,
            directory,
            project: sources.project_name,
            paths,
            own,
            others,
            dependencies,
        })
    }

    pub(super) fn validate(&self, candidate: &str) -> Result<(), CallToolResult> {
        let projected = collect(&self.path, candidate, &self.project, &self.paths)
            .map_err(|error| CallToolResult::error(error.to_string()))?;
        for key in self
            .own
            .keys()
            .chain(projected.keys())
            .collect::<BTreeSet<_>>()
        {
            let before = self.own.get(key).map_or(&[][..], Vec::as_slice);
            let after = projected.get(key).map_or(&[][..], Vec::as_slice);
            if before == after {
                continue;
            }
            let elsewhere = self.others.get(key).map_or(&[][..], Vec::as_slice);
            let previous = before.iter().chain(elsewhere).collect::<Vec<_>>();
            let proposed = after.iter().chain(elsewhere).collect::<Vec<_>>();
            if proposed.len() > 1 && !same_package(&proposed) {
                return Err(self.conflict(key, &proposed, &projected, false));
            }
            if !before.is_empty()
                && !proposed.is_empty()
                && previous.len() > 1
                && same_package(&previous)
                && before.len() > after.len()
            {
                return Err(self.conflict(key, &previous, &projected, true));
            }
        }
        Ok(())
    }

    fn conflict(
        &self,
        key: &Reference,
        placements: &[&Placement],
        projected: &Groups,
        split_package: bool,
    ) -> CallToolResult {
        let reference = format_designator(&key.0, key.1);
        let mut physical = BTreeSet::new();
        let repeated_instance = placements
            .iter()
            .any(|placement| !physical.insert((&placement.document, &placement.symbol.uuid)));
        let suggested_reference = if split_package {
            None
        } else if repeated_instance {
            Some(format!("{}?", key.0))
        } else {
            self.next_free(&key.0, 1, projected)
                .map(|number| format_designator(&key.0, number))
        };
        let details = placements
            .iter()
            .map(|placement| ReferencePlacement {
                schematic: placement.document.display().to_string(),
                instance_path: placement.instance.clone(),
                uuid: placement.symbol.uuid.clone(),
                unit: placement.symbol.unit,
            })
            .collect();
        CallToolResult::error_kind(
            ToolErrorKind::ReferenceConflict {
                reference: reference.clone(),
                placements: details,
                suggested_reference,
            },
            if split_package {
                format!("'{reference}' is a shared package; update its units together")
            } else {
                format!(
                    "'{reference}' conflicts with placed references in project '{}'",
                    self.project
                )
            },
        )
    }

    fn next_free(&self, prefix: &str, first: u32, projected: &Groups) -> Option<u32> {
        let mut key = (prefix.to_owned(), first);
        while self.others.contains_key(&key) || projected.contains_key(&key) {
            key.1 = key.1.checked_add(1)?;
        }
        Some(key.1)
    }

    pub(super) fn template_start(&self) -> u32 {
        self.own
            .keys()
            .map(|(_, number)| *number)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    pub(super) fn next_unused(&self, prefix: &str, first: u32) -> Option<u32> {
        self.next_free(prefix, first, &self.own)
    }

    pub(super) fn commit(self, before: &str, candidate: &str) -> Result<(), SexpError> {
        if before == candidate {
            return Ok(());
        }
        if self.dependencies.is_empty() {
            return write_schematic_if_unchanged(&self.path, before, candidate);
        }
        commit_file_transaction_with_dependencies(
            &self.directory,
            vec![FileTransition::replace(&self.path, before, candidate)],
            self.dependencies,
        )?;
        Ok(())
    }
}

fn same_package(members: &[&Placement]) -> bool {
    one_package(
        &members
            .iter()
            .map(|member| &member.symbol)
            .collect::<Vec<_>>(),
    )
}

fn collect(
    path: &Path,
    source: &str,
    project: &str,
    paths: &BTreeSet<String>,
) -> anyhow::Result<Groups> {
    let mut groups = Groups::new();
    let tree = parse_sexp(source)?;
    let symbols =
        placed_symbols(&tree).map_err(|reason| anyhow::anyhow!("{}: {reason}", path.display()))?;
    for symbol in symbols {
        for instance in paths {
            let mut saved = symbol
                .paths
                .iter()
                .filter(|entry| entry.project == project && entry.path == *instance)
                .peekable();
            // KiCad exports the displayed Reference for an instance without
            // its own saved row, including in an owned project.
            if saved.peek().is_none() {
                add_placement(
                    &mut groups,
                    path,
                    instance,
                    &symbol.property_reference,
                    &symbol,
                );
            }
            for entry in saved {
                add_placement(&mut groups, path, instance, &entry.reference, &symbol);
            }
        }
    }
    Ok(groups)
}

fn add_placement(
    groups: &mut Groups,
    path: &Path,
    instance: &str,
    reference: &str,
    symbol: &super::sch_annotate::PlacedSymbol,
) {
    let designator = parse_designator(reference);
    if let Some(number) = designator.number {
        groups
            .entry((designator.prefix, number))
            .or_default()
            .push(Placement {
                document: path.to_path_buf(),
                instance: instance.to_owned(),
                reference: reference.to_owned(),
                symbol: symbol.clone(),
            });
    }
}
