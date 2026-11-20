//! Resolve placement paths from the project graph and reconcile topology edits.
use super::page_history::{PageHistory, PageHistoryInput};
use super::*;
use crate::tools::{
    project_schematic_roots, resolve_schematic_ownership, resolve_schematic_ownership_with,
    schematic_project_candidates, validate_sheet_instance_state, SheetInstanceContext,
};
use konnect_sexp::transaction::commit_file_transaction_with_dependencies;
use std::collections::BTreeMap;

type Placements = BTreeMap<PathBuf, BTreeSet<String>>;

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct PlacedSheet {
    order: Vec<usize>,
    parent: PathBuf,
    instance: String,
    uuid: String,
}

#[derive(Default)]
struct Forest {
    placements: Placements,
    missing: BTreeSet<PathBuf>,
    roots: Vec<(PathBuf, String)>,
    root_pages: BTreeSet<u32>,
    sheets: Vec<PlacedSheet>,
    max_page: u32,
}

pub(super) struct HierarchySnapshot {
    pub project_name: String,
    pub parent_paths: BTreeSet<String>,
    directory: PathBuf,
    project: Option<(PathBuf, String)>,
    roots: Vec<(PathBuf, Option<String>)>,
    sources: BTreeMap<PathBuf, String>,
    before: Forest,
    max_page: u32,
    page_history: Option<PageHistory>,
}

#[derive(Default)]
pub(super) struct InstanceUpdate {
    pub files: Vec<PathBuf>,
    pub patched_symbols: usize,
    pub patched_sheets: usize,
    pub diagnostics: Vec<String>,
}

impl HierarchySnapshot {
    pub fn load(
        path: &Path,
        before_source: &str,
        base: &str,
        requested_project: Option<&str>,
    ) -> anyhow::Result<Self> {
        let path = path.canonicalize()?;
        let owner = resolve_schematic_ownership(&path)?;
        let (directory, project, project_name, roots) = if let Some(owner) = owner {
            let project = owner.project_file.canonicalize()?;
            let source = read_consistent(&project)?;
            let roots = project_schematic_roots(&project, &source)?;
            let name = project_name_for(&project);
            anyhow::ensure!(
                requested_project.is_none_or(|requested| requested == name),
                "project_name must match the owning project '{name}'"
            );
            (parent_dir(&project), Some((project, source)), name, roots)
        } else {
            let name = requested_project.map_or_else(|| project_name_for(&path), str::to_owned);
            let root_id = parse_sexp(base)?.find_str("uuid").map(str::to_owned);
            (parent_dir(&path), None, name, vec![(path.clone(), root_id)])
        };
        let mut result = Self {
            project_name,
            parent_paths: BTreeSet::new(),
            directory,
            project,
            roots,
            sources: BTreeMap::from([(path.clone(), before_source.to_owned())]),
            before: Forest::default(),
            max_page: 1,
            page_history: None,
        };
        let before = result.walk(&BTreeMap::new(), true)?;
        result.parent_paths = before.placements.get(&path).cloned().ok_or_else(|| {
            anyhow::anyhow!("Project topology changed while reading {}", path.display())
        })?;
        let project = result.project.as_ref().map(|(path, _)| path.as_path());
        let page_history = PageHistory::load(&result.directory, project, before.roots.clone())?;
        result.max_page = before.max_page.max(page_history.high_water());
        result.before = before;
        result.page_history = Some(page_history);
        Ok(result)
    }

    pub fn existing_source(&self, path: &Path) -> anyhow::Result<&str> {
        self.sources
            .get(&normalize_path(path)?)
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("No saved snapshot for {}", path.display()))
    }

    pub fn first_page(&self) -> anyhow::Result<String> {
        Ok(following_page(self.max_page)?.to_string())
    }

    /// The before and projected after forests share saved snapshots. A source
    /// override is never written until all affected instances are verified.
    fn walk(
        &mut self,
        overrides: &BTreeMap<PathBuf, String>,
        reading_before: bool,
    ) -> anyhow::Result<Forest> {
        let mut forest = Forest {
            max_page: 1,
            ..Forest::default()
        };
        let mut root_ids = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for (index, (root, configured_id)) in self.roots.clone().into_iter().enumerate() {
            let root = normalize_path(&root)?;
            let source = self.source(&root, overrides)?;
            let document = cse::Schematic::from_source(&root, source)?;
            let root_id = configured_id
                .or(document.uuid)
                .ok_or_else(|| anyhow::anyhow!("Missing root UUID in {}", root.display()))?;
            anyhow::ensure!(
                root_ids.insert(root_id.clone()),
                "Duplicate top-level sheet UUID {root_id}"
            );
            forest.roots.push((root.clone(), root_id.clone()));
            for footer in parse_sexp(source)?.find_all("sheet_instances") {
                for instance in footer.find_all("path") {
                    if let Some(page) = instance
                        .find_str("page")
                        .and_then(|page| page.parse::<u32>().ok())
                    {
                        forest.max_page = forest.max_page.max(page);
                        if instance.get(1).and_then(|node| node.as_str()) == Some("/") {
                            forest.root_pages.insert(page);
                        }
                    }
                }
            }
            let mut pending = vec![(root, format!("/{root_id}"), Vec::new(), vec![index])];
            while let Some((file, placement, mut ancestors, order)) = pending.pop() {
                anyhow::ensure!(
                    ancestors.len() <= MAX_HIERARCHY_DEPTH && !ancestors.contains(&file),
                    "Cycle or depth limit at {}",
                    file.display()
                );
                anyhow::ensure!(
                    identities.insert(placement.clone()),
                    "Duplicate sheet placement {placement}"
                );
                forest
                    .placements
                    .entry(file.clone())
                    .or_default()
                    .insert(placement.clone());
                let source = self.source(&file, overrides)?;
                let document = cse::Schematic::from_source(&file, source)?;
                ancestors.push(file.clone());
                let survives = reading_before
                    || self
                        .before
                        .placements
                        .get(&file)
                        .is_some_and(|previous| previous.contains(&placement));
                for (index, sheet) in document.sheets.iter().enumerate().rev() {
                    let mut sheet_order = order.clone();
                    sheet_order.push(index);
                    if reading_before {
                        forest.sheets.push(PlacedSheet {
                            parent: file.clone(),
                            instance: placement.clone(),
                            uuid: sheet.uuid.clone(),
                            order: sheet_order.clone(),
                        });
                    }
                    for page in &sheet.instances {
                        if survives
                            && page.project_name == self.project_name
                            && page.path == placement
                        {
                            include_page(&mut forest.max_page, Some(&page.page));
                        }
                    }
                    let child = normalize_path(&parent_dir(&file).join(sheet.file()))?;
                    if !overrides.contains_key(&child) && !child.is_file() {
                        anyhow::ensure!(
                            self.project.is_none()
                                && (reading_before || self.before.missing.contains(&child)),
                            "Referenced schematic file is missing: {}",
                            child.display()
                        );
                        forest.missing.insert(child);
                        continue;
                    }
                    anyhow::ensure!(
                        !sheet.uuid.is_empty(),
                        "Missing sheet UUID in {}",
                        file.display()
                    );
                    pending.push((
                        child,
                        format!("{placement}/{}", sheet.uuid),
                        ancestors.clone(),
                        sheet_order,
                    ));
                }
            }
        }
        Ok(forest)
    }

    fn source<'a>(
        &'a mut self,
        path: &Path,
        overrides: &'a BTreeMap<PathBuf, String>,
    ) -> anyhow::Result<&'a str> {
        if let Some(override_source) = overrides.get(path) {
            return Ok(override_source);
        }
        if !self.sources.contains_key(path) {
            self.sources.insert(path.to_owned(), read_consistent(path)?);
        }
        Ok(&self.sources[path])
    }

    fn verify_no_competing_owner(
        &self,
        path: &Path,
        competing_sources: &mut BTreeMap<PathBuf, String>,
    ) -> anyhow::Result<()> {
        let active = self.project.as_ref().map(|(project, _)| project.as_path());
        let competitor = resolve_schematic_ownership_with(path, active, &mut |input| {
            let input = input.canonicalize()?;
            if let Some(source) = self.sources.get(&input) {
                return Ok(source.clone());
            }
            if !competing_sources.contains_key(&input) {
                competing_sources.insert(input.clone(), read_consistent(&input)?);
            }
            Ok(competing_sources[&input].clone())
        })?;
        if let Some(competitor) = competitor {
            let mut roots = self
                .roots
                .iter()
                .map(|(root, _)| root.clone())
                .collect::<Vec<_>>();
            roots.push(competitor.root_schematic);
            return Err(crate::tools::SchematicTargetError::ProjectConflict {
                target: path.to_path_buf(),
                roots,
            }
            .into());
        }
        Ok(())
    }

    /// Renumber all placements in the proven project forest. Shared physical
    /// files contribute several placements but only one transaction target.
    pub fn renumber_project(mut self) -> anyhow::Result<Vec<Value>> {
        let mut placements = std::mem::take(&mut self.before.sheets);
        placements.sort_unstable();
        let mut documents = BTreeMap::<PathBuf, (cse::Schematic, BTreeSet<String>)>::new();
        let mut report = Vec::new();
        let mut page = 1;
        for placement in placements {
            page = following_page(page)?;
            while self.before.root_pages.contains(&page) {
                page = following_page(page)?;
            }

            let (document, changed) = match documents.entry(placement.parent.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let source = &self.sources[&placement.parent];
                    let schematic = cse::Schematic::from_source(&placement.parent, source)?;
                    entry.insert((schematic, BTreeSet::new()))
                }
            };
            let sheet = document
                .sheets
                .by_uuid_mut(&placement.uuid)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Missing sheet {} in {}",
                        placement.uuid,
                        placement.parent.display()
                    )
                })?;
            let old = sheet.instances.clone();
            let mut seen = BTreeSet::new();
            let live = &self.before.placements[&placement.parent];
            sheet.instances.retain(|row| {
                row.project_name != self.project_name
                    || (live.contains(&row.path) && seen.insert(row.path.clone()))
            });
            let page_text = page.to_string();
            sheet.set_page(&self.project_name, &placement.instance, &page_text);
            if sheet.instances != old {
                changed.insert(placement.uuid);
            }
            report.push(json!({
                "sheet_name": sheet.name(),
                "file": sheet.file(),
                "page": page_text,
                "parent_instance_path": placement.instance,
            }));
        }

        let mut transitions = Vec::new();
        let mut competing_sources = BTreeMap::new();
        for (path, (document, changed)) in documents {
            if changed.is_empty() {
                continue;
            }
            self.verify_no_competing_owner(&path, &mut competing_sources)?;
            let before = &self.sources[&path];
            let ids = changed
                .into_iter()
                .map(ItemId::new)
                .collect::<Result<Vec<_>, _>>()?;
            let command = SchematicCommand::replace_items_from_document(
                before,
                &document.to_source(),
                ids,
                "Renumber project sheet pages",
            )?;
            let (after, _) = prepare_command(&path, before, &command)?;
            transitions.push(FileTransition::replace(path, before, after));
        }
        if !transitions.is_empty() {
            let high_water = self.max_page.max(page);
            self.commit(transitions, competing_sources, high_water)?;
        }
        Ok(report)
    }

    pub fn update(
        mut self,
        parent: &Path,
        parent_after: String,
        created: Option<(PathBuf, String)>,
        repair: Option<bool>,
    ) -> anyhow::Result<InstanceUpdate> {
        let parent = parent.canonicalize()?;
        let mut overrides = BTreeMap::from([(parent.clone(), parent_after)]);
        if let Some((path, source)) = created {
            overrides.insert(normalize_path(&path)?, source);
        }
        let projected = self.walk(&overrides, false)?;
        let candidates = self
            .before
            .placements
            .keys()
            .chain(projected.placements.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut update = InstanceUpdate::default();
        let mut high_water = self.max_page.max(projected.max_page);
        let mut transitions = Vec::new();
        let mut verify = Vec::new();
        let empty = BTreeSet::new();
        let mut competing_sources = BTreeMap::new();

        for path in candidates {
            let prior_paths = self.before.placements.get(&path).unwrap_or(&empty);
            let paths = projected.placements.get(&path).unwrap_or(&empty);
            if !overrides.contains_key(&path) && (repair.is_some() || prior_paths == paths) {
                continue;
            }
            self.verify_no_competing_owner(&path, &mut competing_sources)?;
            let active = self.project.as_ref().map(|(project, _)| project.as_path());
            let foreign_projects = schematic_project_candidates(&path)?
                .iter()
                .filter(|project| active != Some(project.as_path()))
                .map(|project| project_name_for(project))
                .collect::<BTreeSet<_>>();
            let source = overrides
                .get(&path)
                .or_else(|| self.sources.get(&path))
                .ok_or_else(|| anyhow::anyhow!("Missing snapshot for {}", path.display()))?;
            let context = instance_context(&self.project_name, paths);
            if repair.is_some() {
                let sch = cse::Schematic::from_source(&path, source)?;
                if let Err(error) = validate_sheet_instance_state(&path, &sch, &context) {
                    update.diagnostics.push(error.to_string());
                }
            }
            let predecessors = match_placement_moves(prior_paths, paths)?;
            let (after, symbols, sheets) = reconcile(
                &path,
                source,
                &context,
                prior_paths,
                &predecessors,
                &foreign_projects,
                &mut high_water,
            )?;
            update.patched_symbols += symbols;
            update.patched_sheets += sheets;
            if !paths.is_empty() {
                let staged = cse::Schematic::from_source(&path, &after)?;
                validate_sheet_instance_state(&path, &staged, &context)?;
                verify.push((path.clone(), context));
            }
            let transition = match self.sources.get(&path) {
                Some(before) if *before != after => FileTransition::replace(&path, before, after),
                Some(_) => continue,
                None => FileTransition::create(&path, after),
            };
            update.files.push(path);
            transitions.push(transition);
        }

        if !transitions.is_empty() && repair != Some(false) {
            self.commit(transitions, competing_sources, high_water)?;
            for (path, context) in verify {
                let checked = cse::Schematic::load(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|saved| {
                        validate_sheet_instance_state(&path, &saved, &context)
                            .map_err(anyhow::Error::from)
                    });
                if let Err(error) = checked {
                    return Err(konnect_sexp::SexpError::MutationOutcomeUncertain {
                        path,
                        reason: error.to_string(),
                    }
                    .into());
                }
            }
        }
        Ok(update)
    }
    fn commit(
        self,
        mut transitions: Vec<FileTransition>,
        competing_sources: BTreeMap<PathBuf, String>,
        high_water: u32,
    ) -> anyhow::Result<()> {
        let mut dependencies = self
            .sources
            .into_iter()
            .filter(|(path, _)| {
                !transitions
                    .iter()
                    .any(|transition| transition.path() == path)
            })
            .collect::<Vec<_>>();
        dependencies.extend(competing_sources);
        dependencies.extend(self.project);
        let page_history = self.page_history.expect("loaded page history");
        match page_history.commit_input(high_water)? {
            PageHistoryInput::Write(transition) => transitions.push(transition),
            PageHistoryInput::Read(path, source) => dependencies.push((path, source)),
        }
        commit_file_transaction_with_dependencies(&self.directory, transitions, dependencies)?;
        Ok(())
    }
}

fn following_page(page: u32) -> anyhow::Result<u32> {
    page.checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("No numeric sheet pages remain"))
}

fn include_page(max: &mut u32, page: Option<&str>) {
    if let Some(page) = page.and_then(|page| page.parse::<u32>().ok()) {
        *max = (*max).max(page);
    }
}

fn normalize_path(path: &Path) -> anyhow::Result<PathBuf> {
    match path.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or(error)?;
            Ok(parent_dir(path).canonicalize()?.join(name))
        }
        Err(error) => Err(error.into()),
    }
}

fn instance_context(project: &str, paths: &BTreeSet<String>) -> SheetInstanceContext {
    SheetInstanceContext {
        project_name: project.to_owned(),
        instance_path: paths.first().cloned().unwrap_or_default(),
        instance_paths: paths.iter().cloned().collect(),
        is_child_sheet: paths
            .iter()
            .all(|path| path.bytes().filter(|byte| *byte == b'/').count() > 1),
    }
}

/// A single repoint can move occurrences of the same physical document under
/// the edited edge. Match both ways and preserve saved annotation only when
/// the shared ancestor and surviving suffix identify one predecessor.
fn match_placement_moves<'a>(
    before: &'a BTreeSet<String>,
    after: &'a BTreeSet<String>,
) -> anyhow::Result<BTreeMap<&'a str, &'a str>> {
    let removed: Vec<_> = before.difference(after).map(String::as_str).collect();
    let added: Vec<_> = after.difference(before).map(String::as_str).collect();
    let mut moves = BTreeMap::new();
    for new in &added {
        let Some(old) = best_placement(new, &removed)? else {
            continue;
        };
        anyhow::ensure!(
            best_placement(old, &added)? == Some(*new),
            "Ambiguous placement rekey from {old} to {new}"
        );
        moves.insert(*new, old);
    }
    Ok(moves)
}

fn best_placement<'a>(path: &str, candidates: &[&'a str]) -> anyhow::Result<Option<&'a str>> {
    let mut ranked = candidates
        .iter()
        .filter_map(|candidate| {
            let prefix = path
                .split('/')
                .zip(candidate.split('/'))
                .take_while(|(a, b)| a == b)
                .count();
            // An empty segment and the root UUID are insufficient evidence.
            if prefix < 3 {
                return None;
            }
            let suffix = path
                .rsplit('/')
                .zip(candidate.rsplit('/'))
                .take_while(|(a, b)| a == b)
                .count();
            Some(((prefix, suffix), *candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable_by_key(|(score, _)| std::cmp::Reverse(*score));
    if let Some((score, candidate)) = ranked.first() {
        anyhow::ensure!(
            ranked
                .get(1)
                .is_none_or(|(other_score, _)| other_score != score),
            "Ambiguous predecessor for sheet placement {path}"
        );
        Ok(Some(candidate))
    } else {
        Ok(None)
    }
}

fn prior_instance_path<'a>(
    path: &'a str,
    prior_paths: &BTreeSet<String>,
    predecessors: &BTreeMap<&'a str, &'a str>,
) -> Option<&'a str> {
    if prior_paths.contains(path) {
        Some(path)
    } else {
        predecessors.get(path).copied()
    }
}

fn reconcile(
    path: &Path,
    source: &str,
    context: &SheetInstanceContext,
    prior_paths: &BTreeSet<String>,
    predecessors: &BTreeMap<&str, &str>,
    foreign_projects: &BTreeSet<String>,
    high_water: &mut u32,
) -> anyhow::Result<(String, usize, usize)> {
    let mut document = cse::Schematic::from_source(path, source)?;
    let mut changed = Vec::new();
    let mut patched_symbols = 0;
    for symbol in document.symbols.iter_mut() {
        let instances = symbol.instances();
        anyhow::ensure!(
            instances.iter().all(|row| row
                .project
                .as_ref()
                .is_none_or(|project| !foreign_projects.contains(project))),
            "Symbol {} on {} carries a known foreign project's instances",
            symbol.uuid,
            path.display()
        );
        let previous_reference = symbol.reference().map(str::to_owned);
        let desired = if context.instance_paths.is_empty() {
            Vec::new()
        } else {
            let reference = symbol
                .reference()
                .filter(|r| !r.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Symbol {} has no reference", symbol.uuid))?;
            let mut desired = Vec::new();
            for path in &context.instance_paths {
                let saved_at = |path: &str| {
                    instances
                        .iter()
                        .filter(|entry| {
                            entry.project.as_deref() == Some(&context.project_name)
                                && entry.path.as_deref() == Some(path)
                        })
                        .filter_map(|entry| entry.reference.as_deref())
                        .filter(|candidate| {
                            !candidate.is_empty()
                                && reference_prefix(candidate) == reference_prefix(reference)
                        })
                        .collect::<BTreeSet<_>>()
                };
                let saved = prior_instance_path(path, prior_paths, predecessors)
                    .map(saved_at)
                    .unwrap_or_default();
                anyhow::ensure!(
                    saved.len() <= 1,
                    "Conflicting references for symbol {} at {path}",
                    symbol.uuid
                );
                desired.push((
                    path.clone(),
                    saved.first().copied().unwrap_or(reference).to_owned(),
                ));
            }
            if !desired.iter().any(|(_, value)| value == reference) {
                symbol.set_reference(&desired[0].1);
            }
            desired
        };
        if previous_reference.as_deref() == symbol.reference()
            && instances.len() == desired.len()
            && desired.iter().all(|(path, reference)| {
                instances.iter().any(|row| {
                    row.project.as_deref() == Some(&context.project_name)
                        && row.path.as_ref() == Some(path)
                        && row.reference.as_ref() == Some(reference)
                        && row.unit == Some(symbol.unit)
                })
            })
        {
            continue;
        }
        symbol
            .raw_sub_nodes
            .retain(|node| node.tag() != Some("instances"));
        for (path, reference) in desired {
            symbol.set_instance_path(&context.project_name, &path, &reference, symbol.unit);
        }
        patched_symbols += 1;
        changed.push(ItemId::new(symbol.uuid.clone())?);
    }

    let mut patched_sheets = 0;
    for sheet in document.sheets.iter_mut() {
        let instances = sheet.instances.clone();
        anyhow::ensure!(
            instances
                .iter()
                .all(|row| !foreign_projects.contains(&row.project_name)),
            "Sheet {} on {} carries a known foreign project's instances",
            sheet.uuid,
            path.display()
        );
        sheet.instances.clear();
        for path in &context.instance_paths {
            let pages_at = |path: &str| {
                instances
                    .iter()
                    .filter(|row| row.project_name == context.project_name && row.path == path)
                    .map(|row| row.page.as_str())
                    .collect::<BTreeSet<_>>()
            };
            let pages = prior_instance_path(path, prior_paths, predecessors)
                .map(pages_at)
                .unwrap_or_default();
            anyhow::ensure!(
                pages.len() <= 1,
                "Conflicting page numbers for sheet {} at {path}",
                sheet.uuid
            );
            let page = if let Some(page) = pages.first() {
                (*page).to_owned()
            } else {
                *high_water = following_page(*high_water)?;
                high_water.to_string()
            };
            sheet.set_page(&context.project_name, path, &page);
        }
        if instances.len() == sheet.instances.len()
            && sheet.instances.iter().all(|row| instances.contains(row))
        {
            sheet.instances = instances;
        } else {
            patched_sheets += 1;
            changed.push(ItemId::new(sheet.uuid.clone())?);
        }
    }

    if changed.is_empty() {
        return Ok((source.to_owned(), patched_symbols, patched_sheets));
    }
    let command = SchematicCommand::replace_items_from_document(
        source,
        &document.to_source(),
        changed,
        "Reconcile sheet instances",
    )?;
    let (after, _) = prepare_command(path, source, &command)?;
    Ok((after, patched_symbols, patched_sheets))
}

fn reference_prefix(reference: &str) -> &str {
    reference.trim_end_matches(|character: char| character.is_ascii_digit() || character == '?')
}
