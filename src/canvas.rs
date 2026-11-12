//! JSON Canvas 1.0 (<https://jsoncanvas.org/spec/1.0/>), the `.canvas` files of Obsidian, read
//! and written without losing anything of them.
//!
//! Obsidian, its plugins and later versions of the format add fields Rature does not know, and
//! the file is the only truth. So each node and edge is kept whole, as the object it is in the
//! file, its keys in their order, and the typed view (an id, a kind, a place, a colour...) is
//! read from it and written into it: a field Rature does not change is written back as it was,
//! a number too. The top-level keys keep their order, the unknown ones included, and the
//! entries of `nodes` and `edges` are never sorted: the first node is drawn below the others,
//! and a new one goes last, on top.
//!
//! The text written has the layout of the specification's `sample.canvas`, Obsidian's: a tab
//! indent, one node or edge per line, `":"` without a space and no newline at the end, so that a
//! `git diff` shows one line for each card changed; a node or an edge that holds an object or a
//! list (a plugin's field) has one key a line, as Obsidian writes it.
//!
//! A canvas that is not valid (not JSON, a duplicate id, an edge to a node that is not there) is
//! read as far as it can be, with what is wrong with it, and never written.
//!
//! Edited by operations on ids (`canvas_ops`), written by replaying them on the file's text as
//! it is then (`canvas_board`).

use std::collections::HashSet;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Number, Value};

const NODES: &str = "nodes";
const EDGES: &str = "edges";

/// A canvas: its nodes in z order, its edges, and the rest of its top-level object.
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    /// The top-level keys, in the file's order, with their values; `nodes` and `edges` hold
    /// `null` here, their entries being in `nodes` and `edges` below.
    top: Map<String, Value>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// What kept the file from being read whole. A change to the canvas cannot mend it (what
    /// was not read is not in the canvas), so it stays, and the canvas is never written.
    broken: Vec<Problem>,
}

impl Default for Canvas {
    /// An empty canvas, written with an empty `nodes` and an empty `edges`.
    fn default() -> Self {
        let mut top = Map::new();
        top.insert(NODES.into(), Value::Null);
        top.insert(EDGES.into(), Value::Null);
        Self {
            top,
            nodes: Vec::new(),
            edges: Vec::new(),
            broken: Vec::new(),
        }
    }
}

impl Canvas {
    /// Reads the text of a `.canvas` file, as far as it can be read: what is wrong with it is
    /// in [`Canvas::problems`]. An empty file is an empty canvas.
    pub fn read(text: &str) -> Self {
        if text.trim().is_empty() {
            return Self::default();
        }
        let mut canvas = Self {
            top: Map::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            broken: Vec::new(),
        };
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(top)) => canvas.top = top,
            Ok(other) => {
                canvas.broken.push(Problem::Shape(format!(
                    "the file holds {} rather than an object",
                    what(&other)
                )));
                return canvas;
            }
            Err(error) => {
                canvas.broken.push(Problem::Json(error.to_string()));
                return canvas;
            }
        }
        canvas.nodes = take_list(&mut canvas.top, NODES, &mut canvas.broken)
            .into_iter()
            .map(|fields| Node { fields })
            .collect();
        canvas.edges = take_list(&mut canvas.top, EDGES, &mut canvas.broken)
            .into_iter()
            .map(|fields| Edge { fields })
            .collect();
        // Written as a new canvas is, with both lists: the one missing goes last.
        for key in [NODES, EDGES] {
            canvas.top.entry(key).or_insert(Value::Null);
        }
        canvas
    }

    /// The nodes, the first one below the others.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// The nodes, for the operations (`canvas_ops`): their order is the z order, never sorted.
    pub(crate) fn nodes_mut(&mut self) -> &mut Vec<Node> {
        &mut self.nodes
    }

    pub(crate) fn edges_mut(&mut self) -> &mut Vec<Edge> {
        &mut self.edges
    }

    /// The first node with this id.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id() == Some(id))
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|node| node.id() == Some(id))
    }

    /// Puts a node last: on top of the others.
    pub fn push_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    pub fn push_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    /// A new id, 16 hexadecimal digits as Obsidian makes them, that no node or edge has.
    pub fn new_id(&self) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let taken: HashSet<&str> = self
            .nodes
            .iter()
            .filter_map(Node::id)
            .chain(self.edges.iter().filter_map(Edge::id))
            .collect();
        loop {
            // Keys of their own for each `RandomState`, a counter and the time: no two ids alike
            // in this process, and none alike across processes but by chance.
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
            hasher.write_u128(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_nanos()),
            );
            let id = format!("{:016x}", hasher.finish());
            if !taken.contains(id.as_str()) {
                return id;
            }
        }
    }

    /// What is wrong with the canvas, as it is now: empty when it can be written.
    pub fn problems(&self) -> Vec<Problem> {
        let mut problems = self.broken.clone();
        // One set of ids for the nodes and the edges together: each is unique in the file.
        let mut ids = HashSet::new();
        let mut nodes = HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            match node.id() {
                None => problems.push(Problem::NoId {
                    what: "node",
                    index,
                }),
                Some(id) if !ids.insert(id) => problems.push(Problem::DuplicateId {
                    what: "node",
                    id: id.into(),
                }),
                Some(id) => {
                    nodes.insert(id);
                }
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            match edge.id() {
                None => problems.push(Problem::NoId {
                    what: "edge",
                    index,
                }),
                Some(id) if !ids.insert(id) => problems.push(Problem::DuplicateId {
                    what: "edge",
                    id: id.into(),
                }),
                Some(_) => {}
            }
            for (end, node) in [
                ("fromNode", edge.source_node()),
                ("toNode", edge.target_node()),
            ] {
                match node {
                    None => problems.push(Problem::NoEnd { edge: index, end }),
                    Some(node) if !nodes.contains(node) => problems.push(Problem::UnknownNode {
                        edge: index,
                        end,
                        node: node.into(),
                    }),
                    Some(_) => {}
                }
            }
        }
        problems
    }

    /// The text of the canvas, in Obsidian's layout; refused while the canvas is not valid.
    pub fn write(&self) -> Result<String, Vec<Problem>> {
        let problems = self.problems();
        if !problems.is_empty() {
            return Err(problems);
        }
        let mut out = String::from("{");
        for (index, (key, value)) in self.top.iter().enumerate() {
            out.push_str(if index == 0 { "\n\t" } else { ",\n\t" });
            out.push_str(&compact(key));
            out.push(':');
            match key.as_str() {
                NODES => list(&mut out, self.nodes.iter().map(|node| &node.fields)),
                EDGES => list(&mut out, self.edges.iter().map(|edge| &edge.fields)),
                _ => out.push_str(&compact(value)),
            }
        }
        if !self.top.is_empty() {
            out.push('\n');
        }
        out.push('}');
        Ok(out)
    }
}

/// Takes the entries of the list at `key` out of `top`, leaving `null` in its place; what is
/// not a list of objects is told in `broken`, and left out.
fn take_list(
    top: &mut Map<String, Value>,
    key: &str,
    broken: &mut Vec<Problem>,
) -> Vec<Map<String, Value>> {
    let Some(value) = top.get_mut(key) else {
        return Vec::new();
    };
    let Value::Array(entries) = value else {
        broken.push(Problem::Shape(format!(
            "`{key}` is {} rather than an array",
            what(value)
        )));
        return Vec::new();
    };
    let entries = std::mem::take(entries);
    *value = Value::Null;
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            Value::Object(fields) => Some(fields),
            other => {
                broken.push(Problem::Shape(format!(
                    "entry {} of `{key}` is {} rather than an object",
                    index + 1,
                    what(&other)
                )));
                None
            }
        })
        .collect()
}

/// Writes a list of nodes or edges, one a line; an empty one as `[]`.
fn list<'a>(out: &mut String, entries: impl Iterator<Item = &'a Map<String, Value>>) {
    let mut any = false;
    for fields in entries {
        out.push_str(if any { ",\n\t\t" } else { "[\n\t\t" });
        entry(out, fields);
        any = true;
    }
    out.push_str(if any { "\n\t]" } else { "[]" });
}

/// Writes a node or an edge on one line; one that holds an object or a list, as Obsidian
/// writes it, with one key a line, each indented by three tabs, the object or list itself on
/// that line. (Obsidian then leaves a comma after the last key, which makes the file JSON no
/// more: not that.)
fn entry(out: &mut String, fields: &Map<String, Value>) {
    if !fields.values().any(|v| v.is_object() || v.is_array()) {
        out.push_str(&compact(fields));
        return;
    }
    out.push('{');
    for (index, (key, value)) in fields.iter().enumerate() {
        out.push_str(if index == 0 { "\n\t\t\t" } else { ",\n\t\t\t" });
        out.push_str(&compact(key));
        out.push(':');
        out.push_str(&compact(value));
    }
    out.push_str("\n\t\t}");
}

/// JSON on one line, without spaces, its strings escaped as `JSON.stringify` does: `"`, `\` and
/// the control characters, `\n` and the like by name, the others as `\u00XX` in lower case.
fn compact(value: &(impl serde::Serialize + ?Sized)) -> String {
    serde_json::to_string(value).expect("a JSON value, or a string, is always written")
}

fn what(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// What keeps a canvas from being written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The file is not JSON: the parser's message, with its line and column.
    Json(String),
    /// The file is JSON, but not shaped as a canvas.
    Shape(String),
    /// A node or an edge, at this index in its list, without an id.
    NoId { what: &'static str, index: usize },
    /// A node or an edge (`what`) with the id of a node or an edge before it.
    DuplicateId { what: &'static str, id: String },
    /// The edge at this index without its `fromNode` or its `toNode`.
    NoEnd { edge: usize, end: &'static str },
    /// The edge at this index going from or to a node that is not in the canvas.
    UnknownNode {
        edge: usize,
        end: &'static str,
        node: String,
    },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "it is not JSON: {error}"),
            Self::Shape(what) => f.write_str(what),
            Self::NoId { what, index } => write!(f, "{what} {} has no id", index + 1),
            Self::DuplicateId { what, id } => {
                write!(f, "the {what} id {id} is already another node's or edge's")
            }
            Self::NoEnd { edge, end } => write!(f, "edge {} has no {end}", edge + 1),
            Self::UnknownNode { edge, end, node } => write!(
                f,
                "the {end} of edge {}, {node}, is not a node of the canvas",
                edge + 1
            ),
        }
    }
}

/// A node's place and size, in the canvas's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind<'a> {
    Text,
    File,
    Link,
    Group,
    /// A type the specification does not name: a plugin's, or a later version's.
    Other(&'a str),
}

/// A colour: one of the six presets (`"1"` red to `"6"` purple), or `#RRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color<'a> {
    Preset(u8),
    Rgb([u8; 3]),
    /// Neither: kept as it is, drawn with no colour.
    Other(&'a str),
}

impl<'a> Color<'a> {
    fn parse(text: &'a str) -> Self {
        if let [digit @ b'1'..=b'6'] = text.as_bytes() {
            return Self::Preset(digit - b'0');
        }
        let channel = |at: usize| {
            text.get(at..at + 2)
                .filter(|pair| pair.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        };
        match (text.strip_prefix('#'), channel(1), channel(3), channel(5)) {
            (Some(hex), Some(r), Some(g), Some(b)) if hex.len() == 6 => Self::Rgb([r, g, b]),
            _ => Self::Other(text),
        }
    }
}

/// The side of a node an edge leaves or reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side<'a> {
    Top,
    Right,
    Bottom,
    Left,
    Other(&'a str),
}

/// The shape at an end of an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End<'a> {
    /// `none`: the line stops.
    Plain,
    Arrow,
    Other(&'a str),
}

/// A node: its object in the file, whole, and the typed view of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    fields: Map<String, Value>,
}

impl Node {
    /// A new text card, its keys in the order Obsidian writes them; its place and size rounded.
    pub fn new_text(id: &str, text: &str, rect: Rect) -> Self {
        let mut fields = Map::new();
        fields.insert("id".into(), id.into());
        fields.insert("type".into(), "text".into());
        fields.insert("text".into(), text.into());
        let mut node = Self { fields };
        for (key, value) in [
            ("x", rect.x),
            ("y", rect.y),
            ("width", rect.width),
            ("height", rect.height),
        ] {
            node.fields.insert(key.into(), integer(value));
        }
        node
    }

    /// The node's object, as the file has it.
    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    /// The node's object, for the operations (`canvas_ops`).
    pub(crate) fn fields_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.fields
    }

    pub fn id(&self) -> Option<&str> {
        string(&self.fields, "id")
    }

    pub fn kind(&self) -> Option<NodeKind<'_>> {
        Some(match string(&self.fields, "type")? {
            "text" => NodeKind::Text,
            "file" => NodeKind::File,
            "link" => NodeKind::Link,
            "group" => NodeKind::Group,
            other => NodeKind::Other(other),
        })
    }

    /// Its place and size, when all four are numbers.
    pub fn rect(&self) -> Option<Rect> {
        let number = |key| self.fields.get(key).and_then(Value::as_f64);
        Some(Rect {
            x: number("x")?,
            y: number("y")?,
            width: number("width")?,
            height: number("height")?,
        })
    }

    pub fn color(&self) -> Option<Color<'_>> {
        string(&self.fields, "color").map(Color::parse)
    }

    /// A text card's Markdown.
    pub fn text(&self) -> Option<&str> {
        string(&self.fields, "text")
    }

    /// A file card's path.
    pub fn file(&self) -> Option<&str> {
        string(&self.fields, "file")
    }

    /// A file card's heading or block, from its `#`.
    pub fn subpath(&self) -> Option<&str> {
        string(&self.fields, "subpath")
    }

    /// A link card's address.
    pub fn url(&self) -> Option<&str> {
        string(&self.fields, "url")
    }

    /// A group's label.
    pub fn label(&self) -> Option<&str> {
        string(&self.fields, "label")
    }

    /// A group's background image.
    pub fn background(&self) -> Option<&str> {
        string(&self.fields, "background")
    }

    /// How a group's background image fills it: `cover`, `ratio` or `repeat`.
    pub fn background_style(&self) -> Option<&str> {
        string(&self.fields, "backgroundStyle")
    }

    /// Moves the node. Each coordinate that changes is rounded to an integer; one that does
    /// not is left as the file has it.
    pub fn set_position(&mut self, x: f64, y: f64) {
        set_number(&mut self.fields, "x", x);
        set_number(&mut self.fields, "y", y);
    }

    /// Resizes the node, as [`Node::set_position`] moves it.
    pub fn set_size(&mut self, width: f64, height: f64) {
        set_number(&mut self.fields, "width", width);
        set_number(&mut self.fields, "height", height);
    }
}

/// An edge: its object in the file, whole, and the typed view of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    fields: Map<String, Value>,
}

impl Edge {
    /// A new edge from one node to another, with the default ends.
    pub fn new(id: &str, from: &str, to: &str) -> Self {
        let mut fields = Map::new();
        fields.insert("id".into(), id.into());
        fields.insert("fromNode".into(), from.into());
        fields.insert("toNode".into(), to.into());
        Self { fields }
    }

    /// The edge's object, as the file has it.
    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    /// The edge's object, for the operations (`canvas_ops`).
    pub(crate) fn fields_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.fields
    }

    pub fn id(&self) -> Option<&str> {
        string(&self.fields, "id")
    }

    /// Its `fromNode`: the node it starts from (`from_*` names are kept for conversions).
    pub fn source_node(&self) -> Option<&str> {
        string(&self.fields, "fromNode")
    }

    /// Its `toNode`.
    pub fn target_node(&self) -> Option<&str> {
        string(&self.fields, "toNode")
    }

    /// Its `fromSide`.
    pub fn source_side(&self) -> Option<Side<'_>> {
        string(&self.fields, "fromSide").map(side)
    }

    /// Its `toSide`.
    pub fn target_side(&self) -> Option<Side<'_>> {
        string(&self.fields, "toSide").map(side)
    }

    /// Its `fromEnd`, the shape where it starts: none unless the file says otherwise.
    pub fn source_end(&self) -> End<'_> {
        string(&self.fields, "fromEnd").map_or(End::Plain, end)
    }

    /// Its `toEnd`, the shape where it ends: an arrow unless the file says otherwise.
    pub fn target_end(&self) -> End<'_> {
        string(&self.fields, "toEnd").map_or(End::Arrow, end)
    }

    pub fn color(&self) -> Option<Color<'_>> {
        string(&self.fields, "color").map(Color::parse)
    }

    pub fn label(&self) -> Option<&str> {
        string(&self.fields, "label")
    }
}

fn string<'a>(fields: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    fields.get(key).and_then(Value::as_str)
}

fn side(text: &str) -> Side<'_> {
    match text {
        "top" => Side::Top,
        "right" => Side::Right,
        "bottom" => Side::Bottom,
        "left" => Side::Left,
        other => Side::Other(other),
    }
}

fn end(text: &str) -> End<'_> {
    match text {
        "none" => End::Plain,
        "arrow" => End::Arrow,
        other => End::Other(other),
    }
}

/// `value` rounded to the nearest integer (a half away from zero); past the integers JSON can
/// hold here, the nearest of them, and 0 for a value that is not a number.
pub(crate) fn integer(value: f64) -> Value {
    Value::Number(Number::from(value.round() as i64))
}

/// Sets the number at `key`, in its place, rounded, unless it already holds `value`.
fn set_number(fields: &mut Map<String, Value>, key: &str, value: f64) {
    if fields.get(key).and_then(Value::as_f64) != Some(value) {
        fields.insert(key.into(), integer(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The specification's `sample.canvas`, byte for byte: see the README beside it.
    const SAMPLE: &str = include_str!("../tests/fixtures/jsoncanvas/sample.canvas");

    #[test]
    fn the_specifications_sample_is_written_back_byte_for_byte() {
        let canvas = Canvas::read(SAMPLE);
        assert_eq!(canvas.problems(), Vec::<Problem>::new());
        assert_eq!(canvas.write().unwrap(), SAMPLE);
        // The file has the layout this relies on, as fetched: tabs, and no newline at its end.
        assert!(SAMPLE.starts_with("{\n\t\"nodes\":[\n\t\t{\"id\":"));
        assert!(SAMPLE.ends_with("}\n\t]\n}"));
    }

    #[test]
    fn the_sample_reads_into_the_typed_view() {
        let canvas = Canvas::read(SAMPLE);
        let kinds: Vec<_> = canvas.nodes().iter().map(|n| n.kind().unwrap()).collect();
        use NodeKind::*;
        assert_eq!(kinds, [Group, File, File, Text, File]);
        let group = &canvas.nodes()[0];
        assert_eq!(group.label(), Some("JSON Canvas"));
        assert_eq!(
            group.rect(),
            Some(Rect {
                x: -300.,
                y: -460.,
                width: 610.,
                height: 200.
            })
        );
        let readme = canvas.node("8132d4d894c80022").unwrap();
        assert_eq!(readme.file(), Some("readme.md"));
        assert_eq!(readme.color(), Some(Color::Preset(6)));
        let text = canvas.nodes()[3].text().unwrap();
        assert!(text.starts_with("Learn more:\n\n- [Apps]"));
        let edge = &canvas.edges()[0];
        assert_eq!(edge.source_node(), Some("7efdbbe0c4742315"));
        assert_eq!(edge.target_node(), Some("59e896bc8da20699"));
        assert_eq!(edge.source_side(), Some(Side::Right));
        assert_eq!(edge.target_side(), Some(Side::Left));
        assert_eq!(edge.source_end(), End::Plain);
        assert_eq!(edge.target_end(), End::Arrow);
        assert_eq!(edge.label(), None);
    }

    #[test]
    fn unknown_fields_unknown_top_level_keys_and_the_order_of_keys_are_kept() {
        let text = r##"{"meta":{"by":"a plugin","v":[1,2]},"nodes":[{"type":"text","zz":true,"id":"a","text":"é \"q\" \\ / \t \u0001 \u001F €","x":0,"y":0,"width":10,"height":10},{"id":"b","type":"shape","x":20,"y":0,"width":10,"height":10}],"edges":[{"toNode":"a","fromNode":"b","id":"e","bend":0.5}],"after":null}"##;
        let canvas = Canvas::read(text);
        assert_eq!(canvas.problems(), Vec::<Problem>::new());
        assert_eq!(canvas.nodes()[1].kind(), Some(NodeKind::Other("shape")));
        assert_eq!(
            canvas.write().unwrap(),
            concat!(
                "{\n",
                "\t\"meta\":{\"by\":\"a plugin\",\"v\":[1,2]},\n",
                "\t\"nodes\":[\n",
                "\t\t{\"type\":\"text\",\"zz\":true,\"id\":\"a\",\"text\":\"é \\\"q\\\" \\\\ / \\t \\u0001 \\u001f €\",\"x\":0,\"y\":0,\"width\":10,\"height\":10},\n",
                "\t\t{\"id\":\"b\",\"type\":\"shape\",\"x\":20,\"y\":0,\"width\":10,\"height\":10}\n",
                "\t],\n",
                "\t\"edges\":[\n",
                "\t\t{\"toNode\":\"a\",\"fromNode\":\"b\",\"id\":\"e\",\"bend\":0.5}\n",
                "\t],\n",
                "\t\"after\":null\n",
                "}"
            )
        );
    }

    /// As Obsidian writes a node holding an object or a list, after a plugin added one: from
    /// <https://github.com/Developer-Mike/obsidian-advanced-canvas/issues/82#issuecomment-2207926658>.
    #[test]
    fn a_node_holding_an_object_or_a_list_is_written_one_key_a_line() {
        let text = concat!(
            "{\n",
            "\t\"nodes\":[\n",
            "\t\t{\n",
            "\t\t\t\"id\":\"73eb311b22336d56\",\n",
            "\t\t\t\"type\":\"file\",\n",
            "\t\t\t\"file\":\"Any File.md\",\n",
            "\t\t\t\"styleAttributes\":{},\n",
            "\t\t\t\"x\":-880,\n",
            "\t\t\t\"y\":-40,\n",
            "\t\t\t\"width\":360,\n",
            "\t\t\t\"height\":220,\n",
            "\t\t\t\"color\":\"6\"\n",
            "\t\t},\n",
            "\t\t{\"id\":\"9496b3657781a5cd\",\"type\":\"file\",\"file\":\"any file.canvas\",\"x\":-260,\"y\":-320,\"width\":400,\"height\":400},\n",
            "\t\t{\n",
            "\t\t\t\"id\":\"22804ef39aa7c3af\",\n",
            "\t\t\t\"type\":\"text\",\n",
            "\t\t\t\"text\":\"T\",\n",
            "\t\t\t\"tags\":[\"a\",{\"b\":[1]}],\n",
            "\t\t\t\"x\":-880,\n",
            "\t\t\t\"y\":-440,\n",
            "\t\t\t\"width\":360,\n",
            "\t\t\t\"height\":300\n",
            "\t\t}\n",
            "\t],\n",
            "\t\"edges\":[]\n",
            "}"
        );
        let canvas = Canvas::read(text);
        assert_eq!(canvas.problems(), Vec::<Problem>::new());
        assert_eq!(canvas.write().unwrap(), text);
    }

    #[test]
    fn another_layout_is_written_in_obsidians_and_a_new_card_goes_last() {
        let text = "{\n  \"edges\": [],\n  \"nodes\": [\n    {\"id\": \"a\", \"type\": \"text\", \"text\": \"A\", \"x\": 0, \"y\": 0, \"width\": 10, \"height\": 10}\n  ]\n}\n";
        let mut canvas = Canvas::read(text);
        let id = canvas.new_id();
        canvas.push_node(Node::new_text(
            &id,
            "B",
            Rect {
                x: 20.4,
                y: -0.6,
                width: 250.,
                height: 60.5,
            },
        ));
        assert_eq!(
            canvas.write().unwrap(),
            format!(
                "{{\n\t\"edges\":[],\n\t\"nodes\":[\n\t\t{{\"id\":\"a\",\"type\":\"text\",\"text\":\"A\",\"x\":0,\"y\":0,\"width\":10,\"height\":10}},\n\t\t{{\"id\":\"{id}\",\"type\":\"text\",\"text\":\"B\",\"x\":20,\"y\":-1,\"width\":250,\"height\":61}}\n\t]\n}}"
            )
        );
        assert_eq!(
            Canvas::default().write().unwrap(),
            "{\n\t\"nodes\":[],\n\t\"edges\":[]\n}"
        );
        assert_eq!(Canvas::read(" \n"), Canvas::default());
    }

    #[test]
    fn a_number_is_kept_until_it_is_changed_then_rounded() {
        let text = r#"{"nodes":[{"id":"a","type":"text","text":"","x":10.5,"y":-3,"width":100.25,"height":50,"z":1.75}],"edges":[]}"#;
        let mut canvas = Canvas::read(text);
        let untouched = "{\n\t\"nodes\":[\n\t\t{\"id\":\"a\",\"type\":\"text\",\"text\":\"\",\"x\":10.5,\"y\":-3,\"width\":100.25,\"height\":50,\"z\":1.75}\n\t],\n\t\"edges\":[]\n}";
        assert_eq!(canvas.write().unwrap(), untouched);
        // Set to what it holds: nothing changes.
        let node = canvas.node_mut("a").unwrap();
        node.set_position(10.5, -3.);
        node.set_size(100.25, 50.);
        assert_eq!(canvas.write().unwrap(), untouched);
        // Moved right only, grown taller only: the other two are left as they were.
        let node = canvas.node_mut("a").unwrap();
        node.set_position(20.4, -3.);
        node.set_size(100.25, 60.6);
        assert_eq!(
            canvas.write().unwrap(),
            "{\n\t\"nodes\":[\n\t\t{\"id\":\"a\",\"type\":\"text\",\"text\":\"\",\"x\":20,\"y\":-3,\"width\":100.25,\"height\":61,\"z\":1.75}\n\t],\n\t\"edges\":[]\n}"
        );
    }

    #[test]
    fn broken_json_is_read_as_nothing_and_never_written() {
        let canvas = Canvas::read(r#"{"nodes":[{"id":"a","#);
        assert!(canvas.nodes().is_empty());
        let problems = canvas.write().unwrap_err();
        assert!(matches!(problems.as_slice(), [Problem::Json(_)]));
        assert!(problems[0].to_string().starts_with("it is not JSON: "));
        assert!(Canvas::read("[1]").write().is_err());
    }

    #[test]
    fn a_duplicate_id_is_read_and_never_written() {
        let text = r#"{"nodes":[{"id":"a","type":"text","text":"1","x":0,"y":0,"width":1,"height":1},{"id":"a","type":"text","text":"2","x":0,"y":0,"width":1,"height":1}],"edges":[]}"#;
        let canvas = Canvas::read(text);
        assert_eq!(canvas.nodes().len(), 2);
        assert_eq!(
            canvas.write().unwrap_err(),
            [Problem::DuplicateId {
                what: "node",
                id: "a".into()
            }]
        );
    }

    #[test]
    fn an_id_shared_by_a_node_and_an_edge_is_a_duplicate() {
        let text = r#"{"nodes":[{"id":"a","type":"text","text":"","x":0,"y":0,"width":1,"height":1}],"edges":[{"id":"a","fromNode":"a","toNode":"a"}]}"#;
        let problems = Canvas::read(text).write().unwrap_err();
        assert_eq!(
            problems,
            [Problem::DuplicateId {
                what: "edge",
                id: "a".into()
            }]
        );
        assert_eq!(
            problems[0].to_string(),
            "the edge id a is already another node's or edge's"
        );
    }

    #[test]
    fn a_root_without_its_lists_is_written_with_both() {
        assert_eq!(
            Canvas::read("{}").write().unwrap(),
            Canvas::default().write().unwrap()
        );
        assert_eq!(
            Canvas::read(r#"{"meta":1}"#).write().unwrap(),
            "{\n\t\"meta\":1,\n\t\"nodes\":[],\n\t\"edges\":[]\n}"
        );
        assert_eq!(
            Canvas::read(r#"{"edges":[],"meta":1}"#).write().unwrap(),
            "{\n\t\"edges\":[],\n\t\"meta\":1,\n\t\"nodes\":[]\n}"
        );
    }

    #[test]
    fn an_edge_to_a_missing_node_is_read_and_never_written_until_the_node_is_there() {
        let text = r#"{"nodes":[{"id":"a","type":"text","text":"","x":0,"y":0,"width":1,"height":1}],"edges":[{"id":"e","fromNode":"a","toNode":"b"}]}"#;
        let mut canvas = Canvas::read(text);
        assert_eq!(canvas.edges().len(), 1);
        let problems = canvas.write().unwrap_err();
        assert_eq!(
            problems,
            [Problem::UnknownNode {
                edge: 0,
                end: "toNode",
                node: "b".into()
            }]
        );
        assert_eq!(
            problems[0].to_string(),
            "the toNode of edge 1, b, is not a node of the canvas"
        );
        // What the canvas lacks, once given, makes it valid.
        canvas.push_node(Node::new_text(
            "b",
            "",
            Rect {
                x: 0.,
                y: 0.,
                width: 1.,
                height: 1.,
            },
        ));
        assert!(canvas.write().is_ok());
    }

    #[test]
    fn what_was_not_read_keeps_the_canvas_from_being_written() {
        let mut canvas = Canvas::read(r#"{"nodes":[1,{"id":"a","type":"text"}],"edges":{}}"#);
        assert_eq!(canvas.nodes().len(), 1);
        assert_eq!(canvas.problems().len(), 2);
        canvas.push_edge(Edge::new("e", "a", "a"));
        assert!(canvas.write().is_err());
    }

    #[test]
    fn new_ids_are_unique_and_shaped_as_obsidians() {
        let mut canvas = Canvas::read(SAMPLE);
        let mut seen = HashSet::new();
        for _ in 0..2000 {
            let id = canvas.new_id();
            assert_eq!(id.len(), 16);
            assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
            assert!(canvas.node(&id).is_none());
            assert!(seen.insert(id.clone()));
            canvas.push_edge(Edge::new(&id, "754a8ef995f366bc", "8132d4d894c80022"));
        }
        assert_eq!(canvas.problems(), Vec::<Problem>::new());
    }

    #[test]
    fn colours_are_presets_or_hex() {
        assert_eq!(Color::parse("1"), Color::Preset(1));
        assert_eq!(Color::parse("#FF8000"), Color::Rgb([255, 128, 0]));
        assert_eq!(Color::parse("#ff8000"), Color::Rgb([255, 128, 0]));
        for other in ["7", "0", "#FF800", "#FF80000", "red", "#+F8000", "#éé"] {
            assert_eq!(Color::parse(other), Color::Other(other));
        }
    }
}
