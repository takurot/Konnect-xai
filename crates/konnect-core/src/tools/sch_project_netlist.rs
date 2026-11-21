//! Scoped offline connectivity for the saved logical instances of a project.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use konnect_schematic_editor as cse;
use konnect_sexp::{
    geometry::round6,
    schematic::{
        extract_symbol_instances, extract_wires, find_lib_symbol, pin_endpoint, read_schematic,
        LabelKind, LibPin, SymbolInstance,
    },
    SexpNode,
};
use serde_json::{json, Value};

use super::{
    analysis_placed_pins_by_reference, is_power_symbol_reference, resolve_schematic_ownership,
    sch_connectivity::{analysis_net_graph, analysis_net_labels, analysis_net_name, NetRoot},
    sch_hierarchy::MAX_HIERARCHY_DEPTH,
};

struct Snapshot {
    tree: SexpNode,
    schematic: cse::Schematic,
}

struct Sheet {
    saved: Arc<Snapshot>,
    file: PathBuf,
    instance: String,
    name: String,
    parent: Option<(usize, String)>,
}

fn collect_sheets(
    file: &Path,
    mut sheet: SheetIdentity,
    ancestry: &mut Vec<PathBuf>,
    sources: &mut HashMap<PathBuf, Arc<Snapshot>>,
    sheets: &mut Vec<Sheet>,
) -> anyhow::Result<()> {
    let canonical = file.canonicalize()?;
    if ancestry.len() > MAX_HIERARCHY_DEPTH || ancestry.contains(&canonical) {
        bail!("Invalid saved hierarchy at '{}'", file.display());
    }
    let saved = match sources.get(&canonical) {
        Some(saved) => saved.clone(),
        None => {
            let (source, tree) = read_schematic(&canonical)?;
            let schematic = cse::Schematic::from_source(&canonical, source)?;
            verify_snapshot(&canonical, &tree, &schematic)?;
            let saved = Arc::new(Snapshot { tree, schematic });
            sources.insert(canonical.clone(), saved.clone());
            saved
        }
    };
    if sheet.instance.is_empty() {
        let uuid = saved
            .schematic
            .uuid
            .as_deref()
            .context("Missing root UUID")?;
        sheet.instance = format!("/{uuid}");
    }

    let index = sheets.len();
    let dir = canonical
        .parent()
        .context("Schematic has no parent directory")?;
    let mut child_ids = HashSet::new();
    ancestry.push(canonical.clone());
    sheets.push(Sheet {
        saved: saved.clone(),
        file: canonical.clone(),
        instance: sheet.instance.clone(),
        name: sheet.name.clone(),
        parent: sheet.parent,
    });
    for child in saved.schematic.sheets.iter() {
        if child.uuid.is_empty() || !child_ids.insert(&child.uuid) || child.file().is_empty() {
            bail!(
                "Missing or repeated saved child identity in '{}'",
                file.display()
            );
        }
        collect_sheets(
            &dir.join(child.file()),
            SheetIdentity {
                instance: format!("{}/{}", sheet.instance, child.uuid),
                name: format!("{}{}/", sheet.name, child.name()),
                parent: Some((index, child.uuid.clone())),
            },
            ancestry,
            sources,
            sheets,
        )?;
    }
    ancestry.pop();
    Ok(())
}

/// The editor parser can omit incomplete records. Project connectivity needs
/// every placement and every parent-side port before it can name a net.
fn verify_snapshot(path: &Path, tree: &SexpNode, schematic: &cse::Schematic) -> anyhow::Result<()> {
    let sheets = tree.find_all("sheet");
    if sheets.len() != schematic.sheets.len()
        || tree.find_all("symbol").len() != schematic.symbols.len()
        || sheets
            .iter()
            .zip(schematic.sheets.iter())
            .any(|(raw, sheet)| {
                raw.find_str("uuid") != Some(sheet.uuid.as_str())
                    || raw.find_all("pin").len() != sheet.pins.len()
            })
    {
        bail!(
            "Could not read every saved sheet, symbol and sheet pin in '{}'",
            path.display()
        );
    }
    let mut ids = HashSet::new();
    if schematic
        .symbols
        .iter()
        .any(|symbol| symbol.uuid.is_empty() || !ids.insert(&symbol.uuid))
    {
        bail!(
            "Missing or repeated saved symbol UUID in '{}'",
            path.display()
        );
    }
    Ok(())
}

struct SheetIdentity {
    instance: String,
    name: String,
    parent: Option<(usize, String)>,
}

/// Select exact saved occurrence references in the analysis tree's memory view.
fn scoped_tree(sheet: &Sheet, project: &str, reused: bool) -> anyhow::Result<SexpNode> {
    let mut tree = sheet.saved.tree.clone();
    let SexpNode::List(children) = &mut tree else {
        bail!("Invalid schematic '{}'", sheet.file.display());
    };
    let symbols: HashMap<_, _> = sheet
        .saved
        .schematic
        .symbols
        .iter()
        .map(|symbol| (symbol.uuid.as_str(), symbol))
        .collect();
    for node in children
        .iter_mut()
        .filter(|node| node.head() == Some("symbol"))
    {
        let uuid = node.find_str("uuid").context("Missing symbol UUID")?;
        let symbol = symbols.get(uuid).context("Missing saved symbol")?;
        let rows = symbol.instances();
        if rows.is_empty() && !reused {
            continue;
        }
        let mut rows = rows.iter().filter(|row| {
            row.project.as_deref() == Some(project)
                && row.path.as_deref() == Some(sheet.instance.as_str())
        });
        let row = rows
            .next()
            .with_context(|| format!("Symbol {uuid} has no saved instance '{}'", sheet.instance))?;
        if rows.next().is_some() {
            bail!("Symbol {uuid} has repeated instance '{}'", sheet.instance);
        }
        let reference = row
            .reference
            .as_deref()
            .filter(|reference| !reference.is_empty())
            .with_context(|| format!("Symbol {uuid} has a missing instance reference"))?;
        let unit = row.unit.unwrap_or(symbol.unit);
        let SexpNode::List(fields) = node else {
            unreachable!()
        };
        let property = fields.iter_mut().find_map(|field| {
            if field.head() != Some("property") {
                return None;
            }
            let SexpNode::List(items) = field else {
                return None;
            };
            let name = 1 + usize::from(items.get(1).and_then(SexpNode::as_str) == Some("private"));
            (items.get(name).and_then(SexpNode::as_str) == Some("Reference"))
                .then(|| items.get_mut(name + 1))
                .flatten()
        });
        *property.context("Missing saved Reference property")? =
            SexpNode::Str(reference.to_owned());

        let unit_value = SexpNode::Atom(unit.to_string());
        if let Some(SexpNode::List(items)) =
            fields.iter_mut().find(|item| item.head() == Some("unit"))
        {
            *items.get_mut(1).context("Missing symbol unit")? = unit_value;
        } else {
            fields.push(SexpNode::List(vec![
                SexpNode::Atom("unit".to_owned()),
                unit_value,
            ]));
        }
    }
    Ok(tree)
}

#[derive(Default)]
struct ProjectNets {
    parents: Vec<usize>,
    global: HashMap<String, usize>,
    drivers: Vec<(usize, Driver)>,
    components: Vec<Component>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Driver {
    priority: u8,
    depth: usize,
    local_priority: u8,
    name: String,
}

struct Component {
    placed: SymbolInstance,
    sheet: String,
    pins: Vec<(usize, LibPin, f64, f64)>,
}

type Ports = HashMap<(usize, String), BTreeMap<String, usize>>;

impl ProjectNets {
    fn vertex(&mut self, vertices: &mut HashMap<NetRoot, usize>, root: NetRoot) -> usize {
        *vertices.entry(root).or_insert_with(|| {
            let index = self.parents.len();
            self.parents.push(index);
            index
        })
    }

    fn root(&mut self, mut id: usize) -> usize {
        while self.parents[id] != id {
            self.parents[id] = self.parents[self.parents[id]];
            id = self.parents[id];
        }
        id
    }

    fn join(&mut self, first: usize, second: usize) {
        let first = self.root(first);
        let second = self.root(second);
        self.parents[second] = first;
    }

    fn add_sheet(
        &mut self,
        index: usize,
        sheet: &Sheet,
        project: &str,
        reused: bool,
        ports: &mut Ports,
    ) -> anyhow::Result<()> {
        let tree = scoped_tree(sheet, project, reused)?;
        let wires = extract_wires(&tree);
        let labels = analysis_net_labels(&tree);
        let libraries = tree
            .find("lib_symbols")
            .map(|node| node.find_all("symbol"))
            .unwrap_or_default();
        let local_power: HashSet<_> = extract_symbol_instances(&tree)
            .into_iter()
            .filter(|symbol| {
                find_lib_symbol(&libraries, symbol).is_some_and(|library| {
                    library
                        .find("power")
                        .and_then(|power| power.get(1))
                        .and_then(SexpNode::as_str)
                        == Some("local")
                })
            })
            .filter_map(|symbol| symbol.uuid)
            .collect();
        let is_global = |label: &konnect_sexp::schematic::Label| {
            label.kind == LabelKind::GlobalLabel
                || (label.kind == LabelKind::PowerSymbol
                    && !label
                        .uuid
                        .as_ref()
                        .is_some_and(|uuid| local_power.contains(uuid)))
        };
        let scoped: Vec<_> = labels
            .iter()
            .map(|label| {
                let mut scoped = label.clone();
                let kind = if is_global(label) { 'G' } else { 'S' };
                scoped.net = format!("{kind}:{}", label.net);
                scoped
            })
            .collect();
        let mut graph = analysis_net_graph(&tree, &wires, &scoped);
        let mut vertices = HashMap::new();
        let depth = sheet.instance.matches('/').count();
        let mut named_roots = HashSet::new();

        for label in labels {
            let root = graph.root_at(label.x, label.y);
            let id = self.vertex(&mut vertices, root);
            named_roots.insert(root);
            if is_global(&label) {
                if let Some(other) = self.global.insert(label.net.clone(), id) {
                    self.join(id, other);
                }
                let priority = u8::from(label.kind != LabelKind::GlobalLabel);
                self.drivers.push((
                    id,
                    Driver {
                        priority,
                        depth: 0,
                        local_priority: 0,
                        name: label.net,
                    },
                ));
            } else {
                if label.kind == LabelKind::HierarchicalLabel {
                    if let Some(parent) = sheet.parent.as_ref().and_then(|parent| ports.get(parent))
                    {
                        if let Some(&other) = parent.get(&label.net) {
                            self.join(id, other);
                        }
                    }
                }
                let local_priority = match label.kind {
                    LabelKind::PowerSymbol => 0,
                    LabelKind::NetLabel => 1,
                    _ => 2,
                };
                self.drivers.push((
                    id,
                    Driver {
                        priority: 2,
                        depth,
                        local_priority,
                        name: format!("{}{}", sheet.name, label.net),
                    },
                ));
            }
        }
        for child in sheet.saved.schematic.sheets.iter() {
            let mut child_ports = BTreeMap::new();
            for pin in &child.pins {
                let id = self.vertex(&mut vertices, graph.root_at(pin.at.x, pin.at.y));
                if child_ports
                    .insert(analysis_net_name(&pin.name), id)
                    .is_some()
                {
                    bail!(
                        "Repeated parent pin '{}' in sheet '{}'",
                        pin.name,
                        child.name()
                    );
                }
            }
            ports.insert((index, child.uuid.clone()), child_ports);
        }
        let placed_pins = analysis_placed_pins_by_reference(&tree);
        if placed_pins.len() != sheet.saved.schematic.symbols.len() {
            bail!(
                "Could not resolve all embedded symbols in '{}'",
                sheet.file.display()
            );
        }
        for (placed, pins) in placed_pins {
            if is_power_symbol_reference(&placed.reference) {
                continue;
            }
            let pins = pins
                .into_iter()
                .map(|(pin, transform)| {
                    let (x, y) = pin_endpoint(&pin, transform);
                    let root = graph.root_at(x, y);
                    let id = self.vertex(&mut vertices, root);
                    if !named_roots.contains(&root) {
                        if let Some(name) = graph.name_of_root(root) {
                            self.drivers.push((
                                id,
                                Driver {
                                    priority: 3,
                                    depth: 0,
                                    local_priority: 0,
                                    name,
                                },
                            ));
                        }
                    }
                    (id, pin, x, y)
                })
                .collect();
            self.components.push(Component {
                placed,
                sheet: sheet.instance.clone(),
                pins,
            });
        }
        Ok(())
    }

    fn into_summary(mut self) -> Value {
        let mut drivers: BTreeMap<usize, Driver> = BTreeMap::new();
        for (id, candidate) in std::mem::take(&mut self.drivers) {
            let root = self.root(id);
            if drivers.get(&root).is_none_or(|driver| candidate < *driver) {
                drivers.insert(root, candidate);
            }
        }
        let mut nets = BTreeSet::new();
        let components: Vec<_> = std::mem::take(&mut self.components).into_iter().map(|component| {
            let pins: Vec<_> = component.pins.into_iter().map(|(id, pin, x, y)| {
                let name = drivers.get(&self.root(id)).map_or("~", |driver| driver.name.as_str());
                if name != "~" {
                    nets.insert(name.to_owned());
                }
                json!({"number": pin.number, "name": pin.name, "net": name, "x": round6(x), "y": round6(y)})
            }).collect();
            json!({
                "reference": component.placed.reference, "value": component.placed.value,
                "footprint": component.placed.footprint, "lib_id": component.placed.lib_id,
                "sheet_instance_path": component.sheet, "pin_count": pins.len(), "pins": pins,
            })
        }).collect();
        json!({"scope": "project", "component_count": components.len(), "net_count": nets.len(), "nets": nets, "components": components})
    }
}

pub(super) fn project_netlist_summary(path: &Path) -> anyhow::Result<Value> {
    let owner = resolve_schematic_ownership(path)?;
    let (project_file, instance) = if let Some(owner) = owner.as_ref() {
        let [instance] = owner.instance_paths.as_slice() else {
            bail!("Start project analysis at a uniquely identified root schematic");
        };
        if !owner.is_top_level {
            bail!("Start project analysis at the top-level schematic");
        }
        (owner.project_file.as_path(), instance.clone())
    } else {
        (path, String::new())
    };
    let project = project_file
        .file_stem()
        .and_then(|name| name.to_str())
        .context("Missing project name")?;
    let mut sheets = Vec::new();
    collect_sheets(
        path,
        SheetIdentity {
            instance,
            name: "/".to_owned(),
            parent: None,
        },
        &mut Vec::new(),
        &mut HashMap::new(),
        &mut sheets,
    )?;
    let mut counts = HashMap::new();
    for sheet in &sheets {
        *counts.entry(&sheet.file).or_insert(0) += 1;
    }
    let mut nets = ProjectNets::default();
    let mut ports = Ports::new();
    for (index, sheet) in sheets.iter().enumerate() {
        nets.add_sheet(index, sheet, project, counts[&sheet.file] > 1, &mut ports)?;
    }
    Ok(nets.into_summary())
}
