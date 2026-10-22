//! Layout of a run graph on a character grid.
//!
//! The map shows two kinds of structure at once:
//!
//! - **Workflow steps** form a DAG through `needs` edges. They are laid out
//!   as a layered graph from left to right: a step's layer is the longest
//!   `needs` path leading to it, edges that skip layers get one *dummy* slot
//!   per skipped layer, and the order inside each layer is improved with
//!   barycenter sweeps to reduce edge crossings.
//! - **Spawned subagents** hang below their parent's box as an indented tree
//!   (`├─►` / `└─►`), nested to any depth.
//!
//! A node is a *step* (a box in the DAG) when it has no parent, when its
//! parent is a workflow container, or when it takes part in a `needs` edge.
//! Every other node with a parent is a *tree item* under its nearest step.
//! A [`NodeKind::Workflow`] node with children is a *container*: it is not
//! drawn, its children are the steps.
//!
//! **Stability.** [`GraphLayout::compute`] takes the previous layout and
//! seeds each layer with the previous order, appending new nodes at the end.
//! A crossing-reduction pass is only accepted when it strictly lowers the
//! number of crossings, so adding a node never moves existing nodes unless
//! that removes a crossing. [`LayoutCache`] recomputes only when
//! [`RunState::topology_version`] or the density changes; status, cost and
//! token updates reuse the cached geometry.
//!
//! [`Canvas`] draws boxes and orthogonal edges into a clipped grid of
//! themed cells; edge segments that meet are merged into the matching box
//! drawing junction (`┬`, `┤`, `┼`, ...).

use std::collections::{HashMap, HashSet};

use harness_events::reducer::{EdgeKind, RunState};
use harness_events::schema::{NodeId, NodeKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::theme::{Theme, Token};

/// Box size and spacing for one display density.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Density {
    /// Bordered boxes with several lines of detail.
    Full,
    /// One line per node, for large graphs.
    Compact,
}

impl Density {
    /// Width of a step box in cells.
    pub fn node_width(self) -> u16 {
        match self {
            Density::Full => FULL_WIDTH,
            Density::Compact => COMPACT_WIDTH,
        }
    }

    /// Height of a step box in cells.
    pub fn node_height(self) -> u16 {
        match self {
            Density::Full => FULL_HEIGHT,
            Density::Compact => 1,
        }
    }

    /// Blank rows between slots of one layer.
    pub fn row_gap(self) -> u16 {
        match self {
            Density::Full => 1,
            Density::Compact => 0,
        }
    }

    /// Row inside a step box where edges attach.
    pub fn anchor_row(self) -> u16 {
        match self {
            Density::Full => 1,
            Density::Compact => 0,
        }
    }

    /// Column (relative to the box) of the first tree connector.
    pub fn tree_column(self) -> u16 {
        match self {
            Density::Full => 2,
            Density::Compact => 1,
        }
    }
}

/// Width of a full step box.
pub const FULL_WIDTH: u16 = 24;
/// Height of a full step box: border, five content lines, border.
pub const FULL_HEIGHT: u16 = 7;
/// Width of a compact (one-line) node.
pub const COMPACT_WIDTH: u16 = 26;
/// Columns between two layers (edges are routed here).
pub const LAYER_GAP: u16 = 5;
/// Columns a tree level is indented by (`├─► `).
pub const TREE_INDENT: u16 = 4;

/// What a placed box is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    /// A DAG step.
    Step,
    /// A tree item under a step, `depth` >= 1.
    Tree {
        /// Nesting depth below the step.
        depth: u16,
    },
}

/// A node placed on the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeBox {
    /// Node.
    pub id: NodeId,
    /// Cells it occupies. Tree items are one row high and span the label.
    pub rect: Rect,
    /// Layer (column) of the step it belongs to.
    pub layer: usize,
    /// Step or tree item.
    pub kind: BoxKind,
    /// The step this box hangs under (itself for steps).
    pub step: NodeId,
}

/// An orthogonal edge path through grid points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgePath {
    /// Source node.
    pub from: NodeId,
    /// Target node.
    pub to: NodeId,
    /// `needs` or spawn.
    pub kind: EdgeKind,
    /// Corner points; consecutive points share a row or a column.
    pub points: Vec<(u16, u16)>,
    /// Whether the last point gets an arrow head.
    pub arrow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Item {
    Node(NodeId),
    /// Dummy slot of the edge `from -> to` in an intermediate layer.
    Dummy(NodeId, NodeId),
}

/// A computed layout. Coordinates start at (0, 0).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphLayout {
    /// Density the geometry was computed for.
    pub density: Density,
    /// [`RunState::topology_version`] it was computed from.
    pub topology_version: u64,
    /// Every placed node, steps first in layer order, then tree items.
    pub boxes: Vec<NodeBox>,
    /// Every drawn edge.
    pub edges: Vec<EdgePath>,
    /// Total width in cells.
    pub width: u16,
    /// Total height in cells.
    pub height: u16,
    /// Edge crossings left after crossing reduction (needs edges only).
    pub crossings: usize,
    layers: Vec<Vec<Item>>,
    index: HashMap<NodeId, usize>,
}

/// Step / tree classification of a run's nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classified {
    /// DAG steps in order of appearance.
    pub steps: Vec<NodeId>,
    /// Workflow containers (not drawn).
    pub containers: Vec<NodeId>,
    /// Tree items: node -> (step, depth), in pre-order under each step.
    pub trees: Vec<(NodeId, NodeId, u16)>,
}

/// Splits a run's nodes into steps, containers and tree items.
pub fn classify(run: &RunState) -> Classified {
    let mut needs: HashSet<&NodeId> = HashSet::new();
    for e in &run.edges {
        if e.kind == EdgeKind::Needs {
            needs.insert(&e.from);
            needs.insert(&e.to);
        }
    }
    let is_container = |id: &NodeId| {
        run.node(id)
            .is_some_and(|n| n.kind == Some(NodeKind::Workflow) && !n.children.is_empty())
    };
    let mut out = Classified::default();
    let mut step_set: HashSet<NodeId> = HashSet::new();
    for n in &run.nodes {
        if is_container(&n.id) {
            out.containers.push(n.id.clone());
            continue;
        }
        let step = match &n.parent {
            None => true,
            Some(p) => is_container(p) || needs.contains(&n.id),
        };
        if step {
            out.steps.push(n.id.clone());
            step_set.insert(n.id.clone());
        }
    }
    // Pre-order walk of each step's spawned children.
    for s in &out.steps {
        let mut stack: Vec<(NodeId, u16)> = Vec::new();
        if let Some(n) = run.node(s) {
            for c in n.children.iter().rev() {
                stack.push((c.clone(), 1));
            }
        }
        while let Some((id, depth)) = stack.pop() {
            if step_set.contains(&id) || is_container(&id) {
                continue;
            }
            out.trees.push((id.clone(), s.clone(), depth));
            if let Some(n) = run.node(&id) {
                for c in n.children.iter().rev() {
                    stack.push((c.clone(), depth + 1));
                }
            }
        }
    }
    out
}

/// Assigns each step its layer: the longest `needs` path to it. Steps in a
/// cycle are placed after everything else; edges that do not go forward are
/// not drawn.
fn assign_layers(steps: &[NodeId], needs: &[(NodeId, NodeId)]) -> HashMap<NodeId, usize> {
    let pos: HashMap<&NodeId, usize> = steps.iter().enumerate().map(|(i, s)| (s, i)).collect();
    let mut indeg = vec![0usize; steps.len()];
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    for (f, t) in needs {
        if let (Some(&a), Some(&b)) = (pos.get(f), pos.get(t)) {
            out[a].push(b);
            indeg[b] += 1;
        }
    }
    let mut layer = vec![0usize; steps.len()];
    let mut done = vec![false; steps.len()];
    let mut queue: std::collections::VecDeque<usize> =
        (0..steps.len()).filter(|&i| indeg[i] == 0).collect();
    while let Some(i) = queue.pop_front() {
        done[i] = true;
        for &j in &out[i] {
            layer[j] = layer[j].max(layer[i] + 1);
            indeg[j] -= 1;
            if indeg[j] == 0 {
                queue.push_back(j);
            }
        }
    }
    let max = layer.iter().copied().max().unwrap_or(0);
    for i in 0..steps.len() {
        if !done[i] {
            layer[i] = layer[i].max(max + 1);
        }
    }
    steps.iter().cloned().zip(layer).collect()
}

/// Counts crossings between two adjacent layers.
fn crossings_between(upper: &[Item], lower: &[Item], links: &[(Item, Item)]) -> usize {
    let pu: HashMap<&Item, usize> = upper.iter().enumerate().map(|(i, x)| (x, i)).collect();
    let pl: HashMap<&Item, usize> = lower.iter().enumerate().map(|(i, x)| (x, i)).collect();
    let segs: Vec<(usize, usize)> = links
        .iter()
        .filter_map(|(a, b)| Some((*pu.get(a)?, *pl.get(b)?)))
        .collect();
    let mut n = 0;
    for i in 0..segs.len() {
        for j in i + 1..segs.len() {
            let (a, b) = (segs[i], segs[j]);
            if (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1) {
                n += 1;
            }
        }
    }
    n
}

fn total_crossings(layers: &[Vec<Item>], links: &[Vec<(Item, Item)>]) -> usize {
    (1..layers.len())
        .map(|i| crossings_between(&layers[i - 1], &layers[i], &links[i]))
        .sum()
}

/// One barycenter sweep. `down` orders layer i by its neighbours in i-1.
fn sweep(layers: &mut [Vec<Item>], links: &[Vec<(Item, Item)>], down: bool) {
    let n = layers.len();
    let order: Vec<usize> = if down {
        (1..n).collect()
    } else {
        (0..n.saturating_sub(1)).rev().collect()
    };
    for i in order {
        let fixed = if down { i - 1 } else { i + 1 };
        let pos: HashMap<Item, usize> = layers[fixed]
            .iter()
            .enumerate()
            .map(|(k, x)| (x.clone(), k))
            .collect();
        let link_layer = if down { &links[i] } else { &links[i + 1] };
        let mut keyed: Vec<(f64, usize, Item)> = layers[i]
            .iter()
            .enumerate()
            .map(|(k, item)| {
                let nbrs: Vec<usize> = link_layer
                    .iter()
                    .filter_map(|(a, b)| {
                        let (me, other) = if down { (b, a) } else { (a, b) };
                        (me == item).then(|| pos.get(other).copied()).flatten()
                    })
                    .collect();
                let bary = if nbrs.is_empty() {
                    k as f64
                } else {
                    nbrs.iter().sum::<usize>() as f64 / nbrs.len() as f64
                };
                (bary, k, item.clone())
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        layers[i] = keyed.into_iter().map(|(_, _, x)| x).collect();
    }
}

impl GraphLayout {
    /// Lays out `run`. `prev` (the last layout of the same run) seeds the
    /// order inside each layer so existing nodes keep their places.
    pub fn compute(run: &RunState, prev: Option<&GraphLayout>, density: Density) -> GraphLayout {
        let cls = classify(run);
        let step_set: HashSet<&NodeId> = cls.steps.iter().collect();
        let needs: Vec<(NodeId, NodeId)> = run
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Needs && step_set.contains(&e.from) && step_set.contains(&e.to)
            })
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        let layer_of = assign_layers(&cls.steps, &needs);
        let n_layers = layer_of.values().copied().max().map_or(0, |m| m + 1);

        // Items per layer, seeded from the previous order.
        let mut layers: Vec<Vec<Item>> = vec![Vec::new(); n_layers];
        for s in &cls.steps {
            layers[layer_of[s]].push(Item::Node(s.clone()));
        }
        // Links between adjacent layers; links[i] connects layer i-1 to i.
        let mut links: Vec<Vec<(Item, Item)>> = vec![Vec::new(); n_layers.max(1)];
        let mut forward: Vec<(NodeId, NodeId)> = Vec::new();
        for (f, t) in &needs {
            let (lf, lt) = (layer_of[f], layer_of[t]);
            if lt <= lf {
                continue;
            }
            forward.push((f.clone(), t.clone()));
            let mut prev_item = Item::Node(f.clone());
            for l in lf + 1..lt {
                let d = Item::Dummy(f.clone(), t.clone());
                layers[l].push(d.clone());
                links[l].push((prev_item, d.clone()));
                prev_item = d;
            }
            links[lt].push((prev_item, Item::Node(t.clone())));
        }
        if let Some(p) = prev {
            for (l, layer) in layers.iter_mut().enumerate() {
                let old: HashMap<&Item, usize> = p
                    .layers
                    .get(l)
                    .map(|v| v.iter().enumerate().map(|(i, x)| (x, i)).collect())
                    .unwrap_or_default();
                // Stable: old items by old position, new ones after, in
                // their current (appearance) order.
                let mut keyed: Vec<(usize, usize, Item)> = layer
                    .drain(..)
                    .enumerate()
                    .map(|(i, x)| (old.get(&x).copied().unwrap_or(usize::MAX), i, x))
                    .collect();
                keyed.sort_by_key(|k| (k.0, k.1));
                *layer = keyed.into_iter().map(|k| k.2).collect();
            }
        }

        // Crossing reduction: accept only strict improvements.
        let mut best = total_crossings(&layers, &links);
        for round in 0..4 {
            if best == 0 {
                break;
            }
            let mut cand = layers.clone();
            sweep(&mut cand, &links, round % 2 == 0);
            let c = total_crossings(&cand, &links);
            if c < best {
                best = c;
                layers = cand;
            }
        }

        let mut out = GraphLayout {
            density,
            topology_version: run.topology_version,
            boxes: Vec::new(),
            edges: Vec::new(),
            width: 0,
            height: 0,
            crossings: best,
            layers,
            index: HashMap::new(),
        };
        out.place(&cls, &forward, density);
        out
    }

    fn place(&mut self, cls: &Classified, forward: &[(NodeId, NodeId)], d: Density) {
        let w = d.node_width();
        let h = d.node_height();
        let mut tree_rows: HashMap<&NodeId, Vec<(&NodeId, u16)>> = HashMap::new();
        for (id, step, depth) in &cls.trees {
            tree_rows.entry(step).or_default().push((id, *depth));
        }
        let mut dummy_rows: HashMap<(NodeId, NodeId, usize), u16> = HashMap::new();
        let mut height = 0u16;
        let mut tree_boxes = Vec::new();
        let mut tree_edges = Vec::new();
        for (l, layer) in self.layers.iter().enumerate() {
            let x = l as u16 * (w + LAYER_GAP);
            let mut y = 0u16;
            for item in layer {
                match item {
                    Item::Node(id) => {
                        self.boxes.push(NodeBox {
                            id: id.clone(),
                            rect: Rect::new(x, y, w, h),
                            layer: l,
                            kind: BoxKind::Step,
                            step: id.clone(),
                        });
                        let rows = tree_rows.get(id).map(Vec::as_slice).unwrap_or(&[]);
                        let base = y + h;
                        // Connector column per depth: depth 1 hangs off the
                        // box, deeper levels off their parent's glyph.
                        let mut cols: Vec<u16> = vec![x + d.tree_column()];
                        let mut parent_rows: Vec<u16> = vec![y + h - 1];
                        let mut parents: Vec<NodeId> = vec![id.clone()];
                        for (i, (tid, depth)) in rows.iter().enumerate() {
                            let row = base + i as u16;
                            let depth = *depth as usize;
                            cols.truncate(depth);
                            parent_rows.truncate(depth);
                            parents.truncate(depth);
                            let cx = cols[depth - 1];
                            let label_x = cx + TREE_INDENT;
                            let label_w = (x + w + LAYER_GAP - 1).saturating_sub(label_x).max(8);
                            tree_boxes.push(NodeBox {
                                id: (*tid).clone(),
                                rect: Rect::new(label_x, row, label_w, 1),
                                layer: l,
                                kind: BoxKind::Tree {
                                    depth: depth as u16,
                                },
                                step: id.clone(),
                            });
                            tree_edges.push(EdgePath {
                                from: parents[depth - 1].clone(),
                                to: (*tid).clone(),
                                kind: EdgeKind::Spawn,
                                points: vec![
                                    (cx, parent_rows[depth - 1]),
                                    (cx, row),
                                    (label_x - 1, row),
                                ],
                                arrow: true,
                            });
                            cols.push(label_x);
                            parent_rows.push(row);
                            parents.push((*tid).clone());
                        }
                        y = base + rows.len() as u16 + d.row_gap();
                    }
                    Item::Dummy(f, t) => {
                        dummy_rows.insert((f.clone(), t.clone(), l), y + d.anchor_row());
                        y += d.anchor_row() + 1 + d.row_gap();
                    }
                }
            }
            height = height.max(y);
        }
        let layer_count = self.layers.len() as u16;
        self.width = (layer_count * (w + LAYER_GAP)).saturating_sub(LAYER_GAP);
        for (i, b) in self.boxes.iter().enumerate() {
            self.index.insert(b.id.clone(), i);
        }
        // Needs edges, routed through the gaps and dummy rows. Each gap has
        // three turning columns; edges into the same target share one, and
        // edges that turn into a row turn after edges leaving that row, so
        // unrelated edges merge as rarely as possible.
        let mut segs: Vec<(usize, u16, u16, NodeId)> = Vec::new();
        let mut routes: Vec<Route> = Vec::new();
        for (f, t) in forward {
            let (Some(fb), Some(tb)) = (self.get(f), self.get(t)) else {
                continue;
            };
            let mut rows = vec![(fb.layer, fb.rect.y + d.anchor_row())];
            for l in fb.layer + 1..tb.layer {
                rows.push((l, dummy_rows[&(f.clone(), t.clone(), l)]));
            }
            rows.push((tb.layer, tb.rect.y + d.anchor_row()));
            for w in rows.windows(2) {
                if w[0].1 != w[1].1 {
                    segs.push((w[1].0, w[0].1, w[1].1, t.clone()));
                }
            }
            routes.push((f.clone(), t.clone(), rows));
        }
        let mut turn_col: HashMap<(usize, NodeId, u16), u16> = HashMap::new();
        let mut by_gap: HashMap<usize, Vec<(u16, u16, NodeId)>> = HashMap::new();
        for (l, from_row, to_row, t) in segs {
            by_gap.entry(l).or_default().push((from_row, to_row, t));
        }
        for (l, mut v) in by_gap {
            // Down edges with the lowest target first, then up edges with
            // the highest target first.
            v.sort_by_key(|(fr, tr, _)| {
                if tr > fr {
                    (0, u16::MAX - tr)
                } else {
                    (1, *tr)
                }
            });
            let mut slot: HashMap<u16, u16> = HashMap::new();
            for (_, tr, t) in v {
                let next = slot.len() as u16 % 3;
                let c = *slot.entry(tr).or_insert(next);
                turn_col.insert((l, t, tr), c);
            }
        }
        for (f, t, rows) in routes {
            let mut pts = vec![(rows[0].0 as u16 * (w + LAYER_GAP) + w, rows[0].1)];
            for (k, &(l, row)) in rows.iter().enumerate().skip(1) {
                let lx = l as u16 * (w + LAYER_GAP);
                let prev_row = rows[k - 1].1;
                if row != prev_row {
                    let c = turn_col.get(&(l, t.clone(), row)).copied().unwrap_or(1);
                    let mid = lx - LAYER_GAP + 1 + c;
                    pts.push((mid, prev_row));
                    pts.push((mid, row));
                }
                let last = k == rows.len() - 1;
                pts.push(if last { (lx - 1, row) } else { (lx + w, row) });
            }
            self.edges.push(EdgePath {
                from: f,
                to: t,
                kind: EdgeKind::Needs,
                points: dedup(pts),
                arrow: true,
            });
        }
        for b in tree_boxes {
            height = height.max(b.rect.y + 1);
            self.width = self.width.max(b.rect.x + b.rect.width);
            self.index.insert(b.id.clone(), self.boxes.len());
            self.boxes.push(b);
        }
        self.edges.extend(tree_edges);
        self.height = height;
    }

    /// The box of `id`.
    pub fn get(&self, id: &NodeId) -> Option<&NodeBox> {
        self.index.get(id).map(|&i| &self.boxes[i])
    }

    /// Number of layers.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Node ids of layer `l` in display order (dummies omitted).
    pub fn layer_nodes(&self, l: usize) -> Vec<NodeId> {
        self.layers
            .get(l)
            .map(|v| {
                v.iter()
                    .filter_map(|x| match x {
                        Item::Node(id) => Some(id.clone()),
                        Item::Dummy(..) => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the layout fits in `w` x `h` cells.
    pub fn fits(&self, w: u16, h: u16) -> bool {
        self.width <= w && self.height <= h
    }

    /// The box nearest to `from` in direction (`dx`, `dy`), for arrow-key
    /// navigation. Candidates must lie in that direction; the closest by
    /// distance along it (plus twice the sideways offset) wins.
    pub fn neighbour(&self, from: &NodeId, dx: i32, dy: i32) -> Option<&NodeId> {
        let a = self.get(from)?.rect;
        let (ax, ay) = (i32::from(a.x), i32::from(a.y));
        self.boxes
            .iter()
            .filter(|b| &b.id != from)
            .filter_map(|b| {
                let (bx, by) = (i32::from(b.rect.x), i32::from(b.rect.y));
                let (ddx, ddy) = (bx - ax, by - ay);
                let along = ddx * dx + ddy * dy;
                if along <= 0 {
                    return None;
                }
                let side = (ddx * dy).abs() + (ddy * dx).abs();
                Some((along + 2 * side, b.rect.y, b.rect.x, &b.id))
            })
            .min_by_key(|k| (k.0, k.1, k.2))
            .map(|k| k.3)
    }
}

/// An edge and the (layer, row) it passes in each layer.
type Route = (NodeId, NodeId, Vec<(usize, u16)>);

fn dedup(pts: Vec<(u16, u16)>) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

/// Caches the layout of one run and recomputes it only when the topology
/// or the density changes.
#[derive(Debug, Clone, Default)]
pub struct LayoutCache {
    layout: Option<GraphLayout>,
    computes: u64,
}

impl LayoutCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The layout for `run` at `density`, computed only when needed.
    pub fn get(&mut self, run: &RunState, density: Density) -> &GraphLayout {
        let stale = self
            .layout
            .as_ref()
            .is_none_or(|l| l.topology_version != run.topology_version || l.density != density);
        if stale {
            let next = GraphLayout::compute(run, self.layout.as_ref(), density);
            self.layout = Some(next);
            self.computes += 1;
        }
        self.layout.as_ref().expect("layout computed")
    }

    /// The cached layout, if any.
    pub fn current(&self) -> Option<&GraphLayout> {
        self.layout.as_ref()
    }

    /// How many times a layout was computed (for tests and benchmarks).
    pub fn computes(&self) -> u64 {
        self.computes
    }

    /// Drops the cached layout (e.g. when another run is shown).
    pub fn clear(&mut self) {
        self.layout = None;
    }
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

fn junction(bits: u8) -> char {
    match bits {
        b if b == UP | DOWN => '│',
        b if b == LEFT | RIGHT => '─',
        b if b == DOWN | RIGHT => '┌',
        b if b == DOWN | LEFT => '┐',
        b if b == UP | RIGHT => '└',
        b if b == UP | LEFT => '┘',
        b if b == UP | DOWN | RIGHT => '├',
        b if b == UP | DOWN | LEFT => '┤',
        b if b == DOWN | LEFT | RIGHT => '┬',
        b if b == UP | LEFT | RIGHT => '┴',
        b if b == UP | DOWN | LEFT | RIGHT => '┼',
        UP | DOWN => '│',
        _ => '─',
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    style: Style,
    bits: u8,
    /// Second half of a wide character.
    cont: bool,
}

/// A clipped character grid: a viewport onto graph coordinates.
#[derive(Debug, Clone)]
pub struct Canvas {
    view: Rect,
    cells: Vec<Cell>,
    base: Style,
}

impl Canvas {
    /// A blank canvas showing the graph area `view` (graph coordinates).
    pub fn new(view: Rect, base: Style) -> Canvas {
        let blank = Cell {
            ch: ' ',
            style: base,
            bits: 0,
            cont: false,
        };
        Canvas {
            view,
            cells: vec![blank; usize::from(view.width) * usize::from(view.height)],
            base,
        }
    }

    /// The visible graph area.
    pub fn view(&self) -> Rect {
        self.view
    }

    fn idx(&self, x: u16, y: u16) -> Option<usize> {
        let v = self.view;
        (x >= v.x && y >= v.y && x < v.x + v.width && y < v.y + v.height)
            .then(|| usize::from(y - v.y) * usize::from(v.width) + usize::from(x - v.x))
    }

    /// Sets one character.
    pub fn put(&mut self, x: u16, y: u16, ch: char, style: Style) {
        if let Some(i) = self.idx(x, y) {
            self.cells[i] = Cell {
                ch,
                style,
                bits: 0,
                cont: false,
            };
        }
    }

    /// Writes `s` from `(x, y)`, at most `max` cells wide. Returns the
    /// width written.
    pub fn text(&mut self, x: u16, y: u16, s: &str, style: Style, max: u16) -> u16 {
        let mut cx = x;
        let end = x.saturating_add(max);
        for c in s.chars() {
            let cw = c.width().unwrap_or(0) as u16;
            if cw == 0 {
                continue;
            }
            if cx + cw > end {
                break;
            }
            self.put(cx, y, c, style);
            if cw == 2
                && let Some(i) = self.idx(cx + 1, y)
            {
                self.cells[i] = Cell {
                    ch: ' ',
                    style,
                    bits: 0,
                    cont: true,
                };
            }
            cx += cw;
        }
        cx - x
    }

    /// Writes styled spans from `(x, y)` within `max` cells.
    pub fn spans(&mut self, x: u16, y: u16, spans: &[Span<'_>], max: u16) -> u16 {
        let mut used = 0;
        for s in spans {
            if used >= max {
                break;
            }
            used += self.text(x + used, y, &s.content, s.style, max - used);
        }
        used
    }

    /// Fills `rect` with spaces in `style`.
    pub fn fill(&mut self, rect: Rect, style: Style) {
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                self.put(x, y, ' ', style);
            }
        }
    }

    /// Draws a rounded box border.
    pub fn frame(&mut self, r: Rect, style: Style) {
        if r.width < 2 || r.height < 2 {
            return;
        }
        let (x2, y2) = (r.x + r.width - 1, r.y + r.height - 1);
        for x in r.x + 1..x2 {
            self.put(x, r.y, '─', style);
            self.put(x, y2, '─', style);
        }
        for y in r.y + 1..y2 {
            self.put(r.x, y, '│', style);
            self.put(x2, y, '│', style);
        }
        self.put(r.x, r.y, '╭', style);
        self.put(x2, r.y, '╮', style);
        self.put(r.x, y2, '╰', style);
        self.put(x2, y2, '╯', style);
    }

    fn add_bits(&mut self, x: u16, y: u16, bits: u8, style: Style) {
        if let Some(i) = self.idx(x, y) {
            let c = &mut self.cells[i];
            c.bits |= bits;
            c.ch = junction(c.bits);
            c.style = style;
            c.cont = false;
        }
    }

    /// Draws an orthogonal polyline, merging with lines already drawn. With
    /// `arrow`, the last cell becomes `►` (or `▼` when the last segment
    /// goes down).
    pub fn path(&mut self, pts: &[(u16, u16)], style: Style, arrow: bool) {
        for w in pts.windows(2) {
            let ((x1, y1), (x2, y2)) = (w[0], w[1]);
            if y1 == y2 {
                let (a, b) = (x1.min(x2), x1.max(x2));
                for x in a..=b {
                    let mut bits = 0;
                    if x > a {
                        bits |= LEFT;
                    }
                    if x < b {
                        bits |= RIGHT;
                    }
                    self.add_bits(x, y1, bits, style);
                }
            } else {
                let (a, b) = (y1.min(y2), y1.max(y2));
                for y in a..=b {
                    let mut bits = 0;
                    if y > a {
                        bits |= UP;
                    }
                    if y < b {
                        bits |= DOWN;
                    }
                    self.add_bits(x1, y, bits, style);
                }
            }
        }
        if arrow && let Some(&(x, y)) = pts.last() {
            let down = pts.len() >= 2 && pts[pts.len() - 2].0 == x;
            self.put(x, y, if down { '▼' } else { '►' }, style);
        }
    }

    /// The canvas as ratatui lines (one per row).
    pub fn lines(&self) -> Vec<Line<'static>> {
        let w = usize::from(self.view.width);
        let mut out = Vec::with_capacity(usize::from(self.view.height));
        for row in self.cells.chunks(w.max(1)) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut cur = String::new();
            let mut style = self.base;
            for c in row {
                if c.cont {
                    continue;
                }
                if c.style != style && !cur.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut cur), style));
                }
                style = c.style;
                cur.push(c.ch);
            }
            if !cur.is_empty() {
                spans.push(Span::styled(cur, style));
            }
            out.push(Line::from(spans));
        }
        out
    }

    /// The canvas as plain text, trailing spaces trimmed (for tests).
    pub fn plain(&self) -> String {
        self.lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Draws a layout with plain labels (node names) into a canvas covering the
/// whole graph. Used by tests and `harness runs show`-style dumps.
pub fn sketch(layout: &GraphLayout, run: &RunState, theme: &Theme) -> Canvas {
    let view = Rect::new(0, 0, layout.width.max(1), layout.height.max(1));
    let mut c = Canvas::new(view, theme.base());
    let edge = theme.fg(Token::Border);
    for e in &layout.edges {
        c.path(&e.points, edge, e.arrow);
    }
    for b in &layout.boxes {
        let name = run.node(&b.id).map_or(b.id.as_str(), |n| n.name.as_str());
        match (b.kind, layout.density) {
            (BoxKind::Step, Density::Full) => {
                c.fill(b.rect, theme.base());
                c.frame(b.rect, theme.fg(Token::Border));
                c.text(
                    b.rect.x + 1,
                    b.rect.y + 1,
                    name,
                    theme.fg(Token::Agent).add_modifier(Modifier::BOLD),
                    b.rect.width - 2,
                );
            }
            _ => {
                c.text(b.rect.x, b.rect.y, name, theme.fg(Token::Fg), b.rect.width);
            }
        }
    }
    // Tree anchors on box bottoms.
    for e in &layout.edges {
        if e.kind == EdgeKind::Spawn
            && let Some(fb) = layout.get(&e.from)
            && fb.kind == BoxKind::Step
            && layout.density == Density::Full
            && let Some(&(x, y)) = e.points.first()
        {
            c.put(x, y, '┬', edge);
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_events::schema::{EventKind, RunEvent, RunId};

    struct B {
        events: Vec<RunEvent>,
    }

    impl B {
        fn new() -> B {
            B { events: Vec::new() }
        }
        fn push(&mut self, node: &str, parent: Option<&str>, kind: EventKind) -> &mut B {
            let seq = self.events.len() as u64 + 1;
            self.events.push(RunEvent {
                seq,
                ts: time::macros::datetime!(2026-10-22 12:00:00 UTC),
                run_id: RunId(uuid::Uuid::nil()),
                node_id: Some(NodeId::from(node)),
                parent_node_id: parent.map(NodeId::from),
                kind,
            });
            self
        }
        fn step(&mut self, name: &str, needs: &[&str]) -> &mut B {
            self.push(
                name,
                None,
                EventKind::NodeStarted {
                    kind: NodeKind::Agent,
                    name: name.into(),
                    agent: None,
                    model: None,
                    host: None,
                    needs: needs.iter().map(|n| NodeId::from(*n)).collect(),
                    queued: true,
                },
            )
        }
        fn sub(&mut self, name: &str, parent: &str) -> &mut B {
            self.push(
                name,
                Some(parent),
                EventKind::NodeStarted {
                    kind: NodeKind::Subagent,
                    name: name.into(),
                    agent: None,
                    model: None,
                    host: None,
                    needs: vec![],
                    queued: false,
                },
            )
        }
        fn run(&self) -> RunState {
            RunState::from_events(&self.events)
        }
    }

    fn draw(run: &RunState, d: Density) -> String {
        let l = GraphLayout::compute(run, None, d);
        sketch(&l, run, &Theme::default()).plain()
    }

    #[test]
    fn linear_chain() {
        let mut b = B::new();
        b.step("repro", &[])
            .step("prove", &["repro"])
            .step("fix", &["prove"])
            .step("proof", &["fix"]);
        insta::assert_snapshot!("layout_linear", draw(&b.run(), Density::Full));
    }

    #[test]
    fn fan_out_and_fan_in() {
        let mut b = B::new();
        b.step("plan", &[])
            .step("a", &["plan"])
            .step("b", &["plan"])
            .step("c", &["plan"])
            .step("merge", &["a", "b", "c"]);
        let run = b.run();
        let l = GraphLayout::compute(&run, None, Density::Full);
        assert_eq!(l.layer_count(), 3);
        assert_eq!(l.layer_nodes(1).len(), 3);
        insta::assert_snapshot!("layout_fan", draw(&run, Density::Full));
    }

    #[test]
    fn matrix_with_long_edges_and_crossing_reduction() {
        let mut b = B::new();
        // Two independent chains whose middle steps are listed crosswise,
        // plus an edge skipping a layer (gets a dummy slot).
        b.step("x1", &[])
            .step("y1", &[])
            .step("y2", &["y1"])
            .step("x2", &["x1"])
            .step("x3", &["x2"])
            .step("y3", &["y2"])
            .step("all", &["x1", "y3", "x3"]);
        let run = b.run();
        let l = GraphLayout::compute(&run, None, Density::Compact);
        assert_eq!(l.crossings, 0, "crossing reduction removes all crossings");
        insta::assert_snapshot!("layout_matrix", draw(&run, Density::Compact));
    }

    #[test]
    fn crossing_reduction_reorders_a_crossed_layer() {
        let mut b = B::new();
        b.step("a", &[])
            .step("b", &[])
            .step("ca", &["b"])
            .step("cb", &["a"]);
        let run = b.run();
        let l = GraphLayout::compute(&run, None, Density::Compact);
        assert_eq!(l.crossings, 0);
        assert_eq!(
            l.layer_nodes(1),
            vec![NodeId::from("cb"), NodeId::from("ca")]
        );
    }

    #[test]
    fn nested_subagent_trees_attach_to_their_parent() {
        let mut b = B::new();
        b.step("main", &[])
            .sub("researcher", "main")
            .sub("reader", "researcher")
            .sub("writer", "main")
            .step("review", &["main"]);
        let run = b.run();
        let l = GraphLayout::compute(&run, None, Density::Full);
        let r = l.get(&NodeId::from("reader")).unwrap();
        assert_eq!(r.kind, BoxKind::Tree { depth: 2 });
        assert_eq!(r.step, NodeId::from("main"));
        let m = l.get(&NodeId::from("main")).unwrap().rect;
        assert!(r.rect.y > m.y + m.height - 1);
        insta::assert_snapshot!("layout_nested", draw(&run, Density::Full));
        insta::assert_snapshot!("layout_nested_compact", draw(&run, Density::Compact));
    }

    #[test]
    fn workflow_containers_are_not_drawn() {
        let mut b = B::new();
        b.push(
            "wf",
            None,
            EventKind::NodeStarted {
                kind: NodeKind::Workflow,
                name: "wf".into(),
                agent: None,
                model: None,
                host: None,
                needs: vec![],
                queued: false,
            },
        );
        b.push(
            "s1",
            Some("wf"),
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                name: "s1".into(),
                agent: None,
                model: None,
                host: None,
                needs: vec![],
                queued: false,
            },
        );
        let run = b.run();
        let c = classify(&run);
        assert_eq!(c.containers, vec![NodeId::from("wf")]);
        assert_eq!(c.steps, vec![NodeId::from("s1")]);
    }

    #[test]
    fn layout_is_stable_when_nodes_are_added() {
        let mut b = B::new();
        b.step("plan", &[])
            .step("a", &["plan"])
            .step("b", &["plan"]);
        let run1 = b.run();
        let l1 = GraphLayout::compute(&run1, None, Density::Full);
        b.step("c", &["plan"]).sub("helper", "a").step("d", &["b"]);
        let run2 = b.run();
        let l2 = GraphLayout::compute(&run2, Some(&l1), Density::Full);
        for id in ["plan", "a", "b"] {
            let id = NodeId::from(id);
            assert_eq!(
                l1.get(&id).unwrap().rect.x,
                l2.get(&id).unwrap().rect.x,
                "{id}"
            );
        }
        // Order inside the layer is kept: a before b.
        let ya = l2.get(&NodeId::from("a")).unwrap().rect.y;
        let yb = l2.get(&NodeId::from("b")).unwrap().rect.y;
        assert!(ya < yb);
        assert_eq!(l2.layer_nodes(1)[..2], l1.layer_nodes(1)[..]);
    }

    #[test]
    fn cache_recomputes_only_on_topology_change() {
        let mut b = B::new();
        b.step("a", &[]).step("b", &["a"]);
        let mut cache = LayoutCache::new();
        let run = b.run();
        cache.get(&run, Density::Full);
        assert_eq!(cache.computes(), 1);
        // A status-only event: same topology.
        b.push("a", None, EventKind::NodeRunning);
        cache.get(&b.run(), Density::Full);
        assert_eq!(cache.computes(), 1);
        b.step("c", &["b"]);
        cache.get(&b.run(), Density::Full);
        assert_eq!(cache.computes(), 2);
        cache.get(&b.run(), Density::Compact);
        assert_eq!(cache.computes(), 3);
    }

    #[test]
    fn neighbour_follows_arrow_directions() {
        let mut b = B::new();
        b.step("plan", &[])
            .step("a", &["plan"])
            .step("b", &["plan"]);
        let run = b.run();
        let l = GraphLayout::compute(&run, None, Density::Full);
        let plan = NodeId::from("plan");
        let a = NodeId::from("a");
        assert_eq!(l.neighbour(&plan, 1, 0), Some(&a));
        assert_eq!(l.neighbour(&a, 0, 1), Some(&NodeId::from("b")));
        assert_eq!(l.neighbour(&a, -1, 0), Some(&plan));
        assert_eq!(l.neighbour(&plan, -1, 0), None);
    }

    #[test]
    fn junctions_merge() {
        let theme = Theme::default();
        let mut c = Canvas::new(Rect::new(0, 0, 5, 3), theme.base());
        c.path(&[(0, 1), (4, 1)], theme.base(), false);
        c.path(&[(2, 0), (2, 2)], theme.base(), false);
        assert_eq!(c.plain(), "  │\n──┼──\n  │");
    }
}
