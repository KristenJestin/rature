//! Operations on a canvas by `id`, replayed on what the file holds when they are written rather
//! than written over it.
//!
//! An operation is made against the canvas Rature shows, and keeps what it found there: the
//! base. Replayed on the disk's canvas, each field it sets is merged three ways, from the base,
//! Rature's value and the disk's:
//!
//! - the disk did not change the field: Rature's value is written;
//! - the disk changed it and Rature did not: the disk's is kept, nothing to say;
//! - both changed it, to different values: **the disk wins**, and the [`Report`] says so; a
//!   card's text Rature typed goes to [`Report::clipboard`], so that nothing typed is lost;
//! - what the operation is about is no longer on disk: it is dropped, and said (with the text
//!   typed for it, if any);
//! - Rature deletes a card the disk changed: the deletion goes through, its edges with it.
//!
//! Each operation has an inverse, with which undo and redo are operations too, merged as any.

use std::fmt;

use serde_json::{Map, Value};

use crate::canvas::{Canvas, Edge, Node, integer};

/// Whether an operation is about a node or an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Node,
    Edge,
}

/// Where a node or an edge goes in its list: last (on top), first (below the others), or at an
/// index, past the end of a shorter list the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Front,
    Back,
    At(usize),
}

impl Place {
    fn index(self, len: usize) -> usize {
        match self {
            Self::Front => len,
            Self::Back => 0,
            Self::At(index) => index.min(len),
        }
    }
}

/// A field set by an operation: its value in the canvas the operation was made on (`None`: it
/// was not there), and the one Rature gives it (`None`: removed).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub key: String,
    pub base: Option<Value>,
    pub value: Option<Value>,
}

/// An operation on a canvas.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Sets fields of a node or an edge: its place, size, text, colour...
    Set {
        target: Target,
        id: String,
        fields: Vec<Field>,
    },
    /// Adds a node, and edges of its (undoing a deletion gives them back).
    AddNode {
        node: Node,
        place: Place,
        edges: Vec<(Place, Edge)>,
    },
    /// Deletes a node with every edge that starts or ends at it; `place` and `edges` are where
    /// they were, for the inverse.
    DeleteNode {
        node: Node,
        place: Place,
        edges: Vec<(Place, Edge)>,
    },
    AddEdge {
        edge: Edge,
        place: Place,
    },
    DeleteEdge {
        edge: Edge,
        place: Place,
    },
    /// Moves a node in the z order; `from` is where it was, for the inverse.
    Reorder {
        id: String,
        from: Place,
        to: Place,
    },
}

impl Op {
    /// Sets fields of a node or an edge of `canvas`, `None` removing one; the id is never
    /// changed. `None` when there is no such node or edge, or when nothing would change.
    pub fn set<'a>(
        canvas: &Canvas,
        target: Target,
        id: &str,
        changes: impl IntoIterator<Item = (&'a str, Option<Value>)>,
    ) -> Option<Self> {
        let now = entry(canvas, target, id)?;
        let fields: Vec<Field> = changes
            .into_iter()
            .filter(|(key, _)| *key != "id")
            .map(|(key, value)| Field {
                key: key.into(),
                base: now.get(key).cloned(),
                value,
            })
            .filter(|field| !same(field.base.as_ref(), field.value.as_ref()))
            .collect();
        (!fields.is_empty()).then(|| Self::Set {
            target,
            id: id.into(),
            fields,
        })
    }

    /// Moves a node; a coordinate that changes is rounded, as [`Node::set_position`] does.
    pub fn move_node(canvas: &Canvas, id: &str, x: f64, y: f64) -> Option<Self> {
        let now = canvas.node(id)?.fields();
        Self::set(
            canvas,
            Target::Node,
            id,
            [("x", number(now, "x", x)), ("y", number(now, "y", y))],
        )
    }

    /// Resizes a node, as [`Op::move_node`] moves it.
    pub fn resize_node(canvas: &Canvas, id: &str, width: f64, height: f64) -> Option<Self> {
        let now = canvas.node(id)?.fields();
        Self::set(
            canvas,
            Target::Node,
            id,
            [
                ("width", number(now, "width", width)),
                ("height", number(now, "height", height)),
            ],
        )
    }

    /// Sets a text card's Markdown.
    pub fn set_text(canvas: &Canvas, id: &str, text: &str) -> Option<Self> {
        Self::set(canvas, Target::Node, id, [("text", Some(text.into()))])
    }

    /// Sets the colour of a node or an edge (`"1"` to `"6"`, or `#RRGGBB`); `None` removes it.
    pub fn set_color(
        canvas: &Canvas,
        target: Target,
        id: &str,
        color: Option<&str>,
    ) -> Option<Self> {
        Self::set(canvas, target, id, [("color", color.map(Value::from))])
    }

    /// Adds a node on top of the others.
    pub fn add_node(node: Node) -> Self {
        Self::AddNode {
            node,
            place: Place::Front,
            edges: Vec::new(),
        }
    }

    /// Deletes a node of `canvas`, and its edges.
    pub fn delete_node(canvas: &Canvas, id: &str) -> Option<Self> {
        let index = canvas.nodes().iter().position(|n| n.id() == Some(id))?;
        let edges = canvas
            .edges()
            .iter()
            .enumerate()
            .filter(|(_, edge)| touches(edge, id))
            .map(|(index, edge)| (Place::At(index), edge.clone()))
            .collect();
        Some(Self::DeleteNode {
            node: canvas.nodes()[index].clone(),
            place: Place::At(index),
            edges,
        })
    }

    /// Adds an edge, last.
    pub fn add_edge(edge: Edge) -> Self {
        Self::AddEdge {
            edge,
            place: Place::Front,
        }
    }

    pub fn delete_edge(canvas: &Canvas, id: &str) -> Option<Self> {
        let index = canvas.edges().iter().position(|e| e.id() == Some(id))?;
        Some(Self::DeleteEdge {
            edge: canvas.edges()[index].clone(),
            place: Place::At(index),
        })
    }

    /// Brings a node above all the others; `None` when it is there already.
    pub fn to_front(canvas: &Canvas, id: &str) -> Option<Self> {
        let index = canvas.nodes().iter().position(|n| n.id() == Some(id))?;
        (index + 1 < canvas.nodes().len()).then(|| Self::Reorder {
            id: id.into(),
            from: Place::At(index),
            to: Place::Front,
        })
    }

    /// Sends a node below all the others; `None` when it is there already.
    pub fn to_back(canvas: &Canvas, id: &str) -> Option<Self> {
        let index = canvas.nodes().iter().position(|n| n.id() == Some(id))?;
        (index > 0).then(|| Self::Reorder {
            id: id.into(),
            from: Place::At(index),
            to: Place::Back,
        })
    }

    /// The operation that undoes this one.
    pub fn inverse(&self) -> Self {
        match self.clone() {
            Self::Set { target, id, fields } => Self::Set {
                target,
                id,
                fields: fields
                    .into_iter()
                    .map(|Field { key, base, value }| Field {
                        key,
                        base: value,
                        value: base,
                    })
                    .collect(),
            },
            Self::AddNode { node, place, edges } => Self::DeleteNode { node, place, edges },
            Self::DeleteNode { node, place, edges } => Self::AddNode { node, place, edges },
            Self::AddEdge { edge, place } => Self::DeleteEdge { edge, place },
            Self::DeleteEdge { edge, place } => Self::AddEdge { edge, place },
            Self::Reorder { id, from, to } => Self::Reorder {
                id,
                from: to,
                to: from,
            },
        }
    }

    /// Whether `other` sets the same fields of the same node or edge, so that the two can be
    /// one (keys typed one after the other, a card dragged).
    pub(crate) fn joins(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Set { target, id, fields },
                Self::Set {
                    target: t,
                    id: i,
                    fields: f,
                },
            ) => {
                target == t
                    && id == i
                    && fields.len() == f.len()
                    && fields.iter().zip(f).all(|(a, b)| a.key == b.key)
            }
            _ => false,
        }
    }

    /// Makes this operation end where `other`, which [`Op::joins`] it, ends: its base stays.
    pub(crate) fn join(&mut self, other: &Self) {
        if let (Self::Set { fields, .. }, Self::Set { fields: next, .. }) = (self, other) {
            for (field, next) in fields.iter_mut().zip(next) {
                field.value = next.value.clone();
            }
        }
    }

    /// Replays the operation on `canvas`, merged with what changed there since it was made;
    /// what it could not do as Rature did it is told in `report`.
    pub fn apply(&self, canvas: &mut Canvas, report: &mut Report) {
        match self {
            Self::Set { target, id, fields } => {
                let Some(now) = entry_mut(canvas, *target, id) else {
                    report.conflicts.push(Conflict::Gone { id: id.clone() });
                    report.clipboard.extend(typed(fields));
                    return;
                };
                let mut kept = Vec::new();
                for field in fields {
                    let base = field.base.as_ref();
                    let value = field.value.as_ref();
                    let disk = now.get(&field.key);
                    // Rature did not change it, or the disk holds Rature's value already.
                    if same(value, base) || same(disk, value) {
                        continue;
                    }
                    if same(disk, base) {
                        match value {
                            Some(value) => {
                                now.insert(field.key.clone(), value.clone());
                            }
                            None => {
                                now.shift_remove(&field.key);
                            }
                        }
                    } else {
                        kept.push(field.key.clone());
                        report.clipboard.extend(typed(std::slice::from_ref(field)));
                    }
                }
                if !kept.is_empty() {
                    report.conflicts.push(Conflict::Kept {
                        id: id.clone(),
                        fields: kept,
                    });
                }
            }
            Self::AddNode { node, place, edges } => {
                let Some(id) = node.id() else { return };
                if taken(canvas, id) {
                    report.conflicts.push(Conflict::Taken { id: id.into() });
                    return;
                }
                let nodes = canvas.nodes_mut();
                nodes.insert(place.index(nodes.len()), node.clone());
                for (place, edge) in edges {
                    add_edge(canvas, edge, *place, report);
                }
            }
            Self::DeleteNode { node, .. } => {
                let Some(id) = node.id() else { return };
                // Gone already: what was asked is done.
                let nodes = canvas.nodes_mut();
                if let Some(index) = nodes.iter().position(|n| n.id() == Some(id)) {
                    nodes.remove(index);
                }
                canvas.edges_mut().retain(|edge| !touches(edge, id));
            }
            Self::AddEdge { edge, place } => add_edge(canvas, edge, *place, report),
            Self::DeleteEdge { edge, .. } => {
                let edges = canvas.edges_mut();
                if let Some(index) = edges.iter().position(|e| e.id() == edge.id()) {
                    edges.remove(index);
                }
            }
            Self::Reorder { id, to, .. } => {
                let nodes = canvas.nodes_mut();
                let Some(index) = nodes.iter().position(|n| n.id() == Some(id)) else {
                    report.conflicts.push(Conflict::Gone { id: id.clone() });
                    return;
                };
                let node = nodes.remove(index);
                nodes.insert(to.index(nodes.len()), node);
            }
        }
    }
}

/// What an operation, replayed on the disk's canvas, could not do as Rature did it: what the
/// window tells the user, and what it puts on the clipboard.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub conflicts: Vec<Conflict>,
    /// Texts the user typed that the disk did not take: kept for them to paste.
    pub clipboard: Vec<String>,
}

impl Report {
    /// Every operation went through as Rature made it.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty() && self.clipboard.is_empty()
    }
}

/// One thing an operation could not do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// Rature and the disk both changed these fields of `id`: the disk's values are kept.
    Kept { id: String, fields: Vec<String> },
    /// `id` is no longer on disk: what Rature did with it is dropped.
    Gone { id: String },
    /// The id Rature gave a node or an edge is another's on disk now: it is not added.
    Taken { id: String },
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kept { id, fields } => write!(
                f,
                "{id} was changed in the file too: its {} there kept",
                fields.join(", ")
            ),
            Self::Gone { id } => write!(f, "{id} was deleted in the file: the change is dropped"),
            Self::Taken { id } => write!(f, "{id} is already in the file: not added again"),
        }
    }
}

fn entry<'a>(canvas: &'a Canvas, target: Target, id: &str) -> Option<&'a Map<String, Value>> {
    match target {
        Target::Node => canvas.node(id).map(Node::fields),
        Target::Edge => canvas
            .edges()
            .iter()
            .find(|edge| edge.id() == Some(id))
            .map(Edge::fields),
    }
}

fn entry_mut<'a>(
    canvas: &'a mut Canvas,
    target: Target,
    id: &str,
) -> Option<&'a mut Map<String, Value>> {
    match target {
        Target::Node => canvas.node_mut(id).map(Node::fields_mut),
        Target::Edge => canvas
            .edges_mut()
            .iter_mut()
            .find(|edge| edge.id() == Some(id))
            .map(Edge::fields_mut),
    }
}

/// Adds `edge` at `place`, when its id is free and both its nodes are there.
fn add_edge(canvas: &mut Canvas, edge: &Edge, place: Place, report: &mut Report) {
    let Some(id) = edge.id() else { return };
    if taken(canvas, id) {
        report.conflicts.push(Conflict::Taken { id: id.into() });
        return;
    }
    for end in [edge.source_node(), edge.target_node()]
        .into_iter()
        .flatten()
    {
        if canvas.node(end).is_none() {
            report.conflicts.push(Conflict::Gone { id: end.into() });
            return;
        }
    }
    let edges = canvas.edges_mut();
    edges.insert(place.index(edges.len()), edge.clone());
}

/// Whether a node or an edge has this id.
fn taken(canvas: &Canvas, id: &str) -> bool {
    canvas.node(id).is_some() || canvas.edges().iter().any(|edge| edge.id() == Some(id))
}

fn touches(edge: &Edge, node: &str) -> bool {
    edge.source_node() == Some(node) || edge.target_node() == Some(node)
}

/// The text a card was given by these fields, if they change it.
fn typed(fields: &[Field]) -> impl Iterator<Item = String> + '_ {
    fields
        .iter()
        .filter(|field| field.key == "text" && !same(field.base.as_ref(), field.value.as_ref()))
        .filter_map(|field| field.value.as_ref()?.as_str().map(str::to_owned))
}

/// The value of a coordinate set to `value`: the file's own when it is that number already
/// (`10.0` stays as it is written), else `value` rounded.
fn number(fields: &Map<String, Value>, key: &str, value: f64) -> Option<Value> {
    match fields.get(key) {
        Some(now) if now.as_f64() == Some(value) => Some(now.clone()),
        _ => Some(integer(value)),
    }
}

/// Two values alike: numbers by their value (`10` and `10.0`), the rest as JSON.
fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(Value::Number(a)), Some(Value::Number(b))) => a.as_f64() == b.as_f64(),
        (a, b) => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"{"nodes":[{"id":"a","type":"text","text":"A","x":10.0,"y":0,"width":100,"height":50}],"edges":[]}"#;

    #[test]
    fn a_number_written_otherwise_is_the_same_field() {
        let mine = Canvas::read(TEXT);
        // 10.0 in the file, 10 asked: nothing to change.
        assert_eq!(Op::move_node(&mine, "a", 10., 0.), None);
        let op = Op::move_node(&mine, "a", 10., 5.).unwrap();
        // The disk has `10` where the base had `10.0`: it did not change it.
        let mut disk = Canvas::read(&TEXT.replace("10.0", "10"));
        let mut report = Report::default();
        op.apply(&mut disk, &mut report);
        assert!(report.is_clean());
        assert_eq!(disk.node("a").unwrap().fields()["y"], 5);
    }

    #[test]
    fn a_node_added_with_an_id_taken_meanwhile_is_not_added() {
        let mine = Canvas::read(TEXT);
        let mut node = mine.node("a").unwrap().clone();
        node.fields_mut().insert("text".into(), "mine".into());
        let mut disk = Canvas::read(TEXT);
        let mut report = Report::default();
        Op::add_node(node).apply(&mut disk, &mut report);
        assert_eq!(disk, mine);
        assert_eq!(report.conflicts, [Conflict::Taken { id: "a".into() }]);
    }

    #[test]
    fn an_edge_to_a_node_gone_is_not_added() {
        let mut disk = Canvas::read(TEXT);
        let mut report = Report::default();
        Op::add_edge(Edge::new("e", "a", "z")).apply(&mut disk, &mut report);
        assert!(disk.edges().is_empty());
        assert_eq!(report.conflicts, [Conflict::Gone { id: "z".into() }]);
        assert!(disk.write().is_ok());
    }

    #[test]
    fn the_id_is_never_set_and_nothing_to_change_is_no_operation() {
        let canvas = Canvas::read(TEXT);
        assert_eq!(
            Op::set(&canvas, Target::Node, "a", [("id", Some("b".into()))]),
            None
        );
        assert_eq!(Op::set_text(&canvas, "a", "A"), None);
        assert_eq!(Op::set_text(&canvas, "z", "A"), None);
        assert_eq!(Op::to_front(&canvas, "a"), None);
        assert_eq!(Op::to_back(&canvas, "a"), None);
    }
}
