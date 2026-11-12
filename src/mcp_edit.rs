//! `canvas_edit` and `canvas_create`, the tools of `rature mcp` that write.
//!
//! A call's operations are checked first as they are given (their fields and their values),
//! then, under the canvas's lock, against the canvas as the disk holds it: one after the other,
//! each made into an operation of [`crate::canvas_ops`] and applied to the canvas the ones
//! before it leave. All of them go through, or none: the first that cannot is named in the
//! error, and nothing is written. The result is written once, by [`canvas_board::update`] (or
//! [`canvas_board::create`]): the window's own write, under the same lock, so that the window
//! and the server never lose each other's changes.
//!
//! An operation names a node or an edge by its `id`, or by the `ref` an earlier operation of
//! the call gave the one it added. It touches only the fields it names, set on the canvas as it
//! is when it is written: what the user changed elsewhere meanwhile stays.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::canvas::{self, Canvas, Color, Edge, Node, NodeKind, revision};
use crate::canvas_board;
use crate::canvas_ops::{Op, Report, Target};
use crate::fsio::Update;
use crate::mcp::{self, Folders, ToolError};

const OPS: [&str; 8] = [
    "add_node",
    "update_node",
    "delete_node",
    "add_edge",
    "update_edge",
    "delete_edge",
    "to_front",
    "to_back",
];

/// The fields an operation takes, besides `op`.
const ADD_NODE: [&str; 13] = [
    "ref", "type", "text", "file", "subpath", "url", "label", "x", "y", "width", "height", "color",
    "near",
];
const UPDATE_NODE: [&str; 12] = [
    "id", "x", "y", "width", "height", "color", "text", "replace", "label", "url", "file",
    "subpath",
];
const ADD_EDGE: [&str; 9] = [
    "ref", "from", "to", "fromSide", "toSide", "fromEnd", "toEnd", "label", "color",
];
const UPDATE_EDGE: [&str; 9] = [
    "id", "from", "to", "fromSide", "toSide", "fromEnd", "toEnd", "label", "color",
];
const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];
const ENDS: [&str; 2] = ["none", "arrow"];
const COLOR: &str = "\"1\" to \"6\" (red, orange, yellow, green, cyan, purple) or \"#RRGGBB\"";

/// The fields of each type of card, besides those of every card.
const CONTENT: [(&str, &[&str]); 4] = [
    ("text", &["text"]),
    ("file", &["file", "subpath"]),
    ("link", &["url"]),
    ("group", &["label"]),
];

/// What one operation asks, its fields checked.
#[derive(Debug)]
enum Edit {
    AddNode(NewNode),
    UpdateNode {
        id: String,
        /// The fields set (`None`: removed), in the order given.
        fields: Vec<(&'static str, Option<Value>)>,
        /// `old` and `new`.
        replace: Option<(String, String)>,
    },
    DeleteNode {
        id: String,
    },
    AddEdge(NewEdge),
    UpdateEdge {
        id: String,
        from: Option<String>,
        to: Option<String>,
        fields: Vec<(&'static str, Option<Value>)>,
    },
    DeleteEdge {
        id: String,
    },
    ToFront {
        id: String,
    },
    ToBack {
        id: String,
    },
}

#[derive(Debug)]
struct NewNode {
    name: Option<String>,
    kind: &'static str,
    /// Its text, file and subpath, url or label, in the order Obsidian writes them.
    content: Vec<(&'static str, Value)>,
    place: Option<(f64, f64)>,
    width: Option<f64>,
    height: Option<f64>,
    color: Option<String>,
    near: Option<String>,
}

#[derive(Debug)]
struct NewEdge {
    name: Option<String>,
    from: String,
    to: String,
    /// Its sides and ends, label and colour, in the order Obsidian writes them.
    fields: Vec<(&'static str, Value)>,
}

/// One operation of a call, with what names it in an error (`ops[2] (update_node)`).
#[derive(Debug)]
struct Step {
    label: String,
    edit: Edit,
}

impl Step {
    fn fail(&self, code: &'static str, what: String) -> ToolError {
        fail(&self.label, code, what)
    }
}

/// An operation's failure: `what` is a sentence or more.
fn fail(label: &str, code: &'static str, what: String) -> ToolError {
    ToolError::new(code, format!("{label}: {what} Nothing was applied."))
}

/// What the operations did, as the tools give it.
#[derive(Debug, Default)]
struct Applied {
    /// Each `ref` (or, for an addition without one, its label), with the id given.
    created: Map<String, Value>,
    created_ids: HashSet<String>,
    changed: Vec<String>,
    deleted: Vec<String>,
}

impl Applied {
    fn output(mut self, text: &str) -> Value {
        // A node changed, then deleted, is deleted.
        let deleted: HashSet<&String> = self.deleted.iter().collect();
        self.changed.retain(|id| !deleted.contains(id));
        json!({
            "revision": revision(text),
            "created": self.created,
            "changed": self.changed,
            "deleted": self.deleted,
        })
    }

    fn changed(&mut self, id: &str) {
        if !self.created_ids.contains(id) && !self.changed.iter().any(|c| c == id) {
            self.changed.push(id.into());
        }
    }

    fn deleted(&mut self, id: &str) {
        if !self.deleted.iter().any(|d| d == id) {
            self.deleted.push(id.into());
        }
    }
}

pub(crate) fn canvas_edit(
    folders: &Folders,
    arguments: &Map<String, Value>,
) -> Result<Value, ToolError> {
    mcp::only(
        arguments,
        "canvas_edit",
        &["path", "ops", "if_revision"],
        "`path`, `ops` and `if_revision`",
    )?;
    let path = mcp::path_argument(folders, arguments)?;
    let ops = match arguments.get("ops") {
        Some(Value::Array(ops)) if !ops.is_empty() => ops,
        Some(Value::Array(_)) => {
            return Err(ToolError::invalid(
                "`ops` is empty: give at least one operation.".into(),
            ));
        }
        None | Some(Value::Null) => {
            return Err(ToolError::invalid(format!(
                "`ops` is required: a list of operations, each an object with `op`, one of {}.",
                OPS.join(", ")
            )));
        }
        Some(other) => {
            return Err(ToolError::invalid(format!(
                "`ops` must be a list of operations, not {other}."
            )));
        }
    };
    let if_revision = match arguments.get("if_revision") {
        None | Some(Value::Null) => None,
        Some(Value::String(asked)) => Some(asked.as_str()),
        Some(other) => {
            return Err(ToolError::invalid(format!(
                "`if_revision` must be a string, the `revision` canvas_read gave, not {other}."
            )));
        }
    };
    let mut names = HashSet::new();
    let steps = ops
        .iter()
        .enumerate()
        .map(|(index, op)| parse_op(index, op, &mut names))
        .collect::<Result<Vec<_>, _>>()?;

    let real = folders.canvas_file(path)?;
    // What the last try found: the file is read again when it changed meanwhile.
    let mut found = None;
    let update = canvas_board::update(&real, &canvas_board::runtime_locks(), |now| {
        let (result, text) = match edit_text(now, path, if_revision, &steps) {
            Ok((applied, text)) => (Ok(applied), text),
            Err(error) => (Err(error), None),
        };
        found = Some(result);
        text
    })
    .map_err(|error| write_error(path, &error))?;
    let text = match update {
        Update::Busy => return Err(busy(path)),
        Update::Written { after, .. } => after,
        Update::Left(text) => text,
    };
    let applied = found.expect("the edit was asked for the text")?;
    Ok(applied.output(&text))
}

/// The operations applied to `now`, the disk's text: what they did, and the text to write
/// (`None` when it is `now`).
fn edit_text(
    now: &str,
    path: &str,
    if_revision: Option<&str>,
    steps: &[Step],
) -> Result<(Applied, Option<String>), ToolError> {
    if let Some(asked) = if_revision {
        let current = revision(now);
        if asked != current {
            return Err(ToolError::new(
                "stale",
                format!(
                    "{path} changed since revision {asked}: its revision is {current} now. Read it again with canvas_read and make the operations on what it holds, or leave `if_revision` out to have them replayed by id on the file as it is. Nothing was applied."
                ),
            ));
        }
    }
    let mut canvas = Canvas::read(now);
    let problems = canvas.problems();
    if !problems.is_empty() {
        return Err(ToolError::new(
            "not_a_canvas",
            format!(
                "{path} cannot be changed while it is not a valid canvas: {}. Mend the file first. Nothing was applied.",
                said(&problems)
            ),
        ));
    }
    let applied = apply(&mut canvas, steps)?;
    let text = written(&canvas, path)?;
    Ok((applied, (text != now).then_some(text)))
}

pub(crate) fn canvas_create(
    folders: &Folders,
    arguments: &Map<String, Value>,
) -> Result<Value, ToolError> {
    mcp::only(
        arguments,
        "canvas_create",
        &["path", "nodes", "edges"],
        "`path`, `nodes` and `edges`",
    )?;
    let path = mcp::path_argument(folders, arguments)?;
    let mut names = HashSet::new();
    let mut steps = Vec::new();
    for (key, op) in [("nodes", "add_node"), ("edges", "add_edge")] {
        let entries = match arguments.get(key) {
            None | Some(Value::Null) => continue,
            Some(Value::Array(entries)) => entries,
            Some(other) => {
                return Err(ToolError::invalid(format!(
                    "`{key}` must be a list of {op} operations (without `op`), not {other}."
                )));
            }
        };
        for (index, entry) in entries.iter().enumerate() {
            let label = format!("{key}[{index}]");
            let Value::Object(fields) = entry else {
                return Err(fail(
                    &label,
                    "invalid",
                    format!("each entry of `{key}` must be an object, as {op} takes it."),
                ));
            };
            if fields.get("op").is_some_and(|given| given != op) {
                return Err(fail(
                    &label,
                    "invalid",
                    format!("an entry of `{key}` is an {op}: leave `op` out."),
                ));
            }
            let args = Args {
                label: &label,
                fields,
            };
            let edit = match op {
                "add_node" => Edit::AddNode(add_node(&args, &mut names)?),
                _ => Edit::AddEdge(add_edge(&args, &mut names)?),
            };
            steps.push(Step { label, edit });
        }
    }

    let real = folders.new_canvas_file(path)?;
    let mut canvas = Canvas::default();
    let applied = apply(&mut canvas, &steps)?;
    let text = written(&canvas, path)?;
    match canvas_board::create(&real, &canvas_board::runtime_locks(), &text) {
        Ok(Update::Busy) => Err(busy(path)),
        Ok(_) => Ok(applied.output(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(mcp::exists(path)),
        Err(error) => Err(ToolError::io_doing(path, "written", &error)),
    }
}

fn written(canvas: &Canvas, path: &str) -> Result<String, ToolError> {
    canvas.write().map_err(|problems| {
        ToolError::invalid(format!(
            "The operations would leave {path} an invalid canvas: {}. Nothing was applied.",
            said(&problems)
        ))
    })
}

fn said(problems: &[canvas::Problem]) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn busy(path: &str) -> ToolError {
    ToolError::new(
        "busy",
        format!(
            "{path} was being written by another program at each try: nothing was applied. Try again."
        ),
    )
}

fn write_error(path: &str, error: &std::io::Error) -> ToolError {
    if error.kind() == std::io::ErrorKind::InvalidData {
        return ToolError::new(
            "not_a_canvas",
            format!("{path} is not a canvas: it is not UTF-8 text. Nothing was applied."),
        );
    }
    ToolError::io_doing(path, "written", error)
}

/// The fields of one operation, read with errors that name it.
struct Args<'a> {
    label: &'a str,
    fields: &'a Map<String, Value>,
}

impl Args<'_> {
    fn invalid(&self, what: String) -> ToolError {
        fail(self.label, "invalid", what)
    }

    /// Refuses a field the operation does not take.
    fn only(&self, op: &str, allowed: &[&str]) -> Result<(), ToolError> {
        match self
            .fields
            .keys()
            .find(|key| *key != "op" && !allowed.contains(&key.as_str()))
        {
            Some(key) => Err(self.invalid(format!(
                "`{key}` is not a field of {op}, which takes {}.",
                allowed
                    .iter()
                    .map(|field| format!("`{field}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            None => Ok(()),
        }
    }

    fn has(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// A string; `null` or left out, `None`.
    fn string(&self, key: &str) -> Result<Option<String>, ToolError> {
        match self.fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(other) => Err(self.invalid(format!("`{key}` must be a string, not {other}."))),
        }
    }

    /// A string that is not empty: an id, a ref, a path, an address.
    fn name(&self, key: &str) -> Result<Option<String>, ToolError> {
        match self.string(key)? {
            Some(name) if name.is_empty() => {
                Err(self.invalid(format!("`{key}` must not be empty.")))
            }
            name => Ok(name),
        }
    }

    fn required(&self, key: &str, what: &str) -> Result<String, ToolError> {
        self.name(key)?
            .ok_or_else(|| self.invalid(format!("`{key}` is required: {what}.")))
    }

    fn number(&self, key: &str) -> Result<Option<f64>, ToolError> {
        match self.fields.get(key) {
            None => Ok(None),
            Some(value) => value.as_f64().map(Some).ok_or_else(|| {
                self.invalid(format!(
                    "`{key}` must be a number (canvas pixels), not {value}."
                ))
            }),
        }
    }

    /// A width or a height: a whole number of pixels, one at least.
    fn size(&self, key: &str) -> Result<Option<f64>, ToolError> {
        match self.number(key)? {
            Some(size) if size.round() < 1. => Err(self.invalid(format!(
                "`{key}` must be 1 or more (canvas pixels), not {size}."
            ))),
            size => Ok(size.map(f64::round)),
        }
    }

    fn color(&self) -> Result<Option<String>, ToolError> {
        match self.string("color")? {
            Some(color) if matches!(Color::parse(&color), Color::Other(_)) => {
                Err(self.invalid(format!("`color` must be {COLOR}, not {color:?}.")))
            }
            color => Ok(color),
        }
    }

    /// One of `allowed`.
    fn one_of(&self, key: &str, allowed: &[&str]) -> Result<Option<String>, ToolError> {
        match self.string(key)? {
            Some(value) if !allowed.contains(&value.as_str()) => Err(self.invalid(format!(
                "`{key}` must be one of {}, not {value:?}.",
                allowed.join(", ")
            ))),
            value => Ok(value),
        }
    }

    fn subpath(&self) -> Result<Option<String>, ToolError> {
        match self.string("subpath")? {
            Some(subpath) if !subpath.starts_with('#') => Err(self.invalid(format!(
                "`subpath` must begin with # (a heading or a block of the file: \"#Heading\"), not {subpath:?}."
            ))),
            subpath => Ok(subpath),
        }
    }

    /// The `ref` an addition is given, once in the call.
    fn reference(&self, names: &mut HashSet<String>) -> Result<Option<String>, ToolError> {
        let Some(name) = self.name("ref")? else {
            return Ok(None);
        };
        if name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
        {
            return Err(self.invalid(format!(
                "`ref` must be 1 to 64 letters, digits or _ - . :, not {name:?}."
            )));
        }
        if !names.insert(name.clone()) {
            return Err(self.invalid(format!(
                "the ref {name} is given twice in this call: give each addition its own."
            )));
        }
        Ok(Some(name))
    }

    /// A field an update sets, or removes with `null`.
    fn set_or_remove(
        &self,
        key: &'static str,
        read: impl Fn(&Self) -> Result<Option<String>, ToolError>,
        fields: &mut Vec<(&'static str, Option<Value>)>,
    ) -> Result<(), ToolError> {
        if self.has(key) {
            fields.push((key, read(self)?.map(Value::from)));
        }
        Ok(())
    }

    /// A field an update sets, which cannot be removed.
    fn set(
        &self,
        key: &'static str,
        read: impl Fn(&Self) -> Result<Option<String>, ToolError>,
        fields: &mut Vec<(&'static str, Option<Value>)>,
        what: &str,
    ) -> Result<(), ToolError> {
        if self.has(key) {
            let value = read(self)?
                .ok_or_else(|| self.invalid(format!("`{key}` cannot be removed: give {what}.")))?;
            fields.push((key, Some(value.into())));
        }
        Ok(())
    }
}

/// Reads operation `index` of `ops`.
fn parse_op(index: usize, op: &Value, names: &mut HashSet<String>) -> Result<Step, ToolError> {
    let at = format!("ops[{index}]");
    let Value::Object(fields) = op else {
        return Err(fail(
            &at,
            "invalid",
            format!(
                "an operation must be an object with `op`, one of {}.",
                OPS.join(", ")
            ),
        ));
    };
    let name = match fields.get("op") {
        Some(Value::String(name)) if OPS.contains(&name.as_str()) => name.as_str(),
        other => {
            return Err(fail(
                &at,
                "invalid",
                format!(
                    "`op` must be one of {}, not {}.",
                    OPS.join(", "),
                    other.map_or("nothing".into(), Value::to_string)
                ),
            ));
        }
    };
    let label = format!("{at} ({name})");
    let args = Args {
        label: &label,
        fields,
    };
    let id = |args: &Args| -> Result<String, ToolError> {
        args.only(name, &["id"])?;
        args.required(
            "id",
            "the id of the node or edge, or the ref of one added before",
        )
    };
    let edit = match name {
        "add_node" => Edit::AddNode(add_node(&args, names)?),
        "update_node" => update_node(&args)?,
        "delete_node" => Edit::DeleteNode { id: id(&args)? },
        "add_edge" => Edit::AddEdge(add_edge(&args, names)?),
        "update_edge" => update_edge(&args)?,
        "delete_edge" => Edit::DeleteEdge { id: id(&args)? },
        "to_front" => Edit::ToFront { id: id(&args)? },
        _ => Edit::ToBack { id: id(&args)? },
    };
    Ok(Step { label, edit })
}

fn add_node(args: &Args, names: &mut HashSet<String>) -> Result<NewNode, ToolError> {
    args.only("add_node", &ADD_NODE)?;
    let types = CONTENT.map(|(kind, _)| kind);
    let kind = match args.string("type")? {
        Some(kind) => match types.iter().find(|t| **t == kind) {
            Some(kind) => *kind,
            None => {
                return Err(args.invalid(format!(
                    "`type` must be one of {}, not {kind:?}.",
                    types.join(", ")
                )));
            }
        },
        None => {
            return Err(args.invalid(format!("`type` is required: one of {}.", types.join(", "))));
        }
    };
    let own = CONTENT
        .iter()
        .find(|(t, _)| *t == kind)
        .map_or(&[][..], |(_, fields)| *fields);
    for (other, fields) in CONTENT {
        for field in fields {
            if other != kind && !own.contains(field) && args.has(field) {
                return Err(args.invalid(format!(
                    "`{field}` is a field of a {other} card, and this one is a {kind} card, which takes {}.",
                    own.iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    }
    let mut content = Vec::new();
    match kind {
        "text" => {
            let text = args.string("text")?.ok_or_else(|| {
                args.invalid("`text` is required for a text card: its Markdown.".into())
            })?;
            content.push(("text", text.into()));
        }
        "file" => {
            let file = args.required("file", "the path of the file the card shows")?;
            content.push(("file", file.into()));
            if let Some(subpath) = args.subpath()? {
                content.push(("subpath", subpath.into()));
            }
        }
        "link" => content.push(("url", args.required("url", "the address shown")?.into())),
        _ => {
            if let Some(label) = args.string("label")? {
                content.push(("label", label.into()));
            }
        }
    }
    let place = match (args.number("x")?, args.number("y")?) {
        (Some(x), Some(y)) => Some((x.round(), y.round())),
        (None, None) => None,
        _ => {
            return Err(args
                .invalid("give both `x` and `y`, or neither, and Rature places the card.".into()));
        }
    };
    let near = args.name("near")?;
    if near.is_some() && place.is_some() {
        return Err(args.invalid(
            "`near` places a card that has no `x` and `y`: give one or the other.".into(),
        ));
    }
    Ok(NewNode {
        name: args.reference(names)?,
        kind,
        content,
        place,
        width: args.size("width")?,
        height: args.size("height")?,
        color: args.color()?,
        near,
    })
}

fn update_node(args: &Args) -> Result<Edit, ToolError> {
    args.only("update_node", &UPDATE_NODE)?;
    let id = args.required("id", "the id of the node, or the ref of one added before")?;
    let mut fields = Vec::new();
    // A number is required where one is given: `null` is refused, as any other value.
    for key in ["x", "y"] {
        if let Some(value) = args.number(key)? {
            fields.push((key, Some(canvas::integer(value))));
        }
    }
    for key in ["width", "height"] {
        if let Some(value) = args.size(key)? {
            fields.push((key, Some(canvas::integer(value))));
        }
    }
    args.set_or_remove("color", Args::color, &mut fields)?;
    args.set("text", |a| a.string("text"), &mut fields, "a text")?;
    args.set_or_remove("label", |a| a.string("label"), &mut fields)?;
    args.set("url", |a| a.name("url"), &mut fields, "an address")?;
    args.set("file", |a| a.name("file"), &mut fields, "a path")?;
    args.set_or_remove("subpath", Args::subpath, &mut fields)?;
    let replace = match args.fields.get("replace") {
        None | Some(Value::Null) => None,
        Some(Value::Object(replace)) => {
            let label = format!("{} `replace`", args.label);
            let inner = Args {
                label: &label,
                fields: replace,
            };
            inner.only("replace", &["old", "new"])?;
            let old = inner.required("old", "the words to change, as they are in the text")?;
            let new = inner.string("new")?.ok_or_else(|| {
                inner.invalid(
                    "`new` is required: the words to put instead (\"\" deletes them).".into(),
                )
            })?;
            Some((old, new))
        }
        Some(other) => {
            return Err(args.invalid(format!(
                "`replace` must be an object {{\"old\": ..., \"new\": ...}}, not {other}."
            )));
        }
    };
    if replace.is_some() && args.has("text") {
        return Err(args
            .invalid("give `text` (the whole text) or `replace` (words in it), not both.".into()));
    }
    if fields.is_empty() && replace.is_none() {
        return Err(args.invalid(format!(
            "nothing to change: give at least one of {}.",
            UPDATE_NODE[1..]
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(Edit::UpdateNode {
        id,
        fields,
        replace,
    })
}

fn add_edge(args: &Args, names: &mut HashSet<String>) -> Result<NewEdge, ToolError> {
    args.only("add_edge", &ADD_EDGE)?;
    let from = args.required(
        "from",
        "the node it starts from, by its id or the ref of one added before",
    )?;
    let to = args.required(
        "to",
        "the node it goes to, by its id or the ref of one added before",
    )?;
    let mut fields = Vec::new();
    for (key, read) in edge_fields() {
        if let Some(value) = read(args)? {
            fields.push((key, value.into()));
        }
    }
    Ok(NewEdge {
        name: args.reference(names)?,
        from,
        to,
        fields,
    })
}

type Read = fn(&Args) -> Result<Option<String>, ToolError>;

/// An edge's fields besides its ends, in the order Obsidian writes them (after `fromNode` and
/// `toNode` for the sides).
fn edge_fields() -> [(&'static str, Read); 6] {
    [
        ("fromSide", |a| a.one_of("fromSide", &SIDES)),
        ("toSide", |a| a.one_of("toSide", &SIDES)),
        ("fromEnd", |a| a.one_of("fromEnd", &ENDS)),
        ("toEnd", |a| a.one_of("toEnd", &ENDS)),
        ("color", |a| a.color()),
        ("label", |a| a.string("label")),
    ]
}

fn update_edge(args: &Args) -> Result<Edit, ToolError> {
    args.only("update_edge", &UPDATE_EDGE)?;
    let id = args.required("id", "the id of the edge, or the ref of one added before")?;
    let mut ends = [None, None];
    for (end, key) in ends.iter_mut().zip(["from", "to"]) {
        if args.has(key) {
            *end = Some(args.name(key)?.ok_or_else(|| {
                args.invalid(format!(
                    "`{key}` cannot be removed: an edge joins two nodes."
                ))
            })?);
        }
    }
    let [from, to] = ends;
    let mut fields = Vec::new();
    for (key, read) in edge_fields() {
        args.set_or_remove(key, read, &mut fields)?;
    }
    if fields.is_empty() && from.is_none() && to.is_none() {
        return Err(args.invalid(format!(
            "nothing to change: give at least one of {}.",
            UPDATE_EDGE[1..]
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(Edit::UpdateEdge {
        id,
        from,
        to,
        fields,
    })
}

/// The refs of a call, with what each names.
type Refs = HashMap<String, (Target, String)>;

/// Applies each step to `canvas`, in order; the first that cannot be is the error, and the
/// canvas is then to be dropped.
fn apply(canvas: &mut Canvas, steps: &[Step]) -> Result<Applied, ToolError> {
    let mut refs = Refs::new();
    let mut applied = Applied::default();
    for step in steps {
        match &step.edit {
            Edit::AddNode(new) => {
                let id = free_ref(canvas, step, new.name.as_deref())?;
                let kind = match new.kind {
                    "text" => NodeKind::Text,
                    "file" => NodeKind::File,
                    "link" => NodeKind::Link,
                    _ => NodeKind::Group,
                };
                let text = new
                    .content
                    .iter()
                    .find(|(key, _)| *key == "text")
                    .and_then(|(_, text)| text.as_str());
                let (width, height) = canvas::default_size(kind, text);
                let (width, height) = (new.width.unwrap_or(width), new.height.unwrap_or(height));
                let (x, y) = match new.place {
                    Some(place) => place,
                    None => {
                        let near = match &new.near {
                            Some(near) => {
                                let near = node_id(canvas, &refs, step, "near", near)?;
                                canvas.node(&near).and_then(Node::rect)
                            }
                            None => None,
                        };
                        canvas.free_place(width, height, near)
                    }
                };
                let mut fields = Map::new();
                fields.insert("id".into(), id.clone().into());
                fields.insert("type".into(), new.kind.into());
                for (key, value) in &new.content {
                    fields.insert((*key).into(), value.clone());
                }
                for (key, value) in [("x", x), ("y", y), ("width", width), ("height", height)] {
                    fields.insert(key.into(), canvas::integer(value));
                }
                if let Some(color) = &new.color {
                    fields.insert("color".into(), color.clone().into());
                }
                run(canvas, step, Op::add_node(Node::from_fields(fields)))?;
                added(&mut applied, &mut refs, step, &new.name, Target::Node, id);
            }
            Edit::UpdateNode {
                id,
                fields,
                replace,
            } => {
                let id = node_id(canvas, &refs, step, "id", id)?;
                let node = canvas.node(&id).expect("a node found");
                let kind = kind_name(node);
                let mut fields = fields.clone();
                for (key, _) in &fields {
                    if let Some((needed, _)) = CONTENT
                        .iter()
                        .find(|(_, own)| own.contains(key))
                        .filter(|(needed, _)| *needed != kind)
                    {
                        return Err(step.fail(
                            "invalid",
                            format!(
                                "`{key}` is a field of a {needed} card, and {id} is a {kind} card."
                            ),
                        ));
                    }
                }
                if let Some((old, new)) = replace {
                    if kind != "text" {
                        return Err(step.fail(
                            "invalid",
                            format!(
                                "`replace` changes a text card's text, and {id} is a {kind} card."
                            ),
                        ));
                    }
                    let text = node.text().unwrap_or_default();
                    match occurrences(text, old) {
                        0 => {
                            return Err(step.fail(
                                "not_found",
                                format!(
                                    "`replace`: {old:?} is not in the text of {id} now (the user may have changed it). Read it again with canvas_read and its `ids`."
                                ),
                            ));
                        }
                        1 => fields.push(("text", Some(text.replacen(old, new, 1).into()))),
                        _ => {
                            return Err(step.fail(
                                "invalid",
                                format!(
                                    "`replace`: {old:?} is in the text of {id} more than once. Give `old` with more of the words around it, so that it is there once."
                                ),
                            ));
                        }
                    }
                }
                if let Some(op) = Op::set(canvas, Target::Node, &id, fields) {
                    run(canvas, step, op)?;
                    applied.changed(&id);
                }
            }
            Edit::DeleteNode { id } => {
                let id = node_id(canvas, &refs, step, "id", id)?;
                let op = Op::delete_node(canvas, &id).expect("a node found");
                if let Op::DeleteNode { edges, .. } = &op {
                    applied.deleted(&id);
                    for (_, edge) in edges {
                        if let Some(edge) = edge.id() {
                            applied.deleted(edge);
                        }
                    }
                }
                run(canvas, step, op)?;
            }
            Edit::AddEdge(new) => {
                let from = node_id(canvas, &refs, step, "from", &new.from)?;
                let to = node_id(canvas, &refs, step, "to", &new.to)?;
                let id = free_ref(canvas, step, new.name.as_deref())?;
                let mut fields = Map::new();
                fields.insert("id".into(), id.clone().into());
                fields.insert("fromNode".into(), from.into());
                let side = |key| new.fields.iter().find(|(k, _)| *k == key);
                if let Some((_, side)) = side("fromSide") {
                    fields.insert("fromSide".into(), side.clone());
                }
                fields.insert("toNode".into(), to.into());
                for (key, value) in &new.fields {
                    if *key != "fromSide" {
                        fields.insert((*key).into(), value.clone());
                    }
                }
                run(canvas, step, Op::add_edge(Edge::from_fields(fields)))?;
                added(&mut applied, &mut refs, step, &new.name, Target::Edge, id);
            }
            Edit::UpdateEdge {
                id,
                from,
                to,
                fields,
            } => {
                let id = edge_id(canvas, &refs, step, id)?;
                let mut changes = fields.clone();
                // An end is checked before it is set: an edge to no node would leave the
                // canvas one that is never written.
                for (key, field, end) in [("from", "fromNode", from), ("to", "toNode", to)] {
                    if let Some(end) = end {
                        let node = node_id(canvas, &refs, step, key, end)?;
                        changes.push((field, Some(node.into())));
                    }
                }
                if let Some(op) = Op::set(canvas, Target::Edge, &id, changes) {
                    run(canvas, step, op)?;
                    applied.changed(&id);
                }
            }
            Edit::DeleteEdge { id } => {
                let id = edge_id(canvas, &refs, step, id)?;
                let op = Op::delete_edge(canvas, &id).expect("an edge found");
                run(canvas, step, op)?;
                applied.deleted(&id);
            }
            Edit::ToFront { id } | Edit::ToBack { id } => {
                let id = node_id(canvas, &refs, step, "id", id)?;
                let op = match step.edit {
                    Edit::ToFront { .. } => Op::to_front(canvas, &id),
                    _ => Op::to_back(canvas, &id),
                };
                if let Some(op) = op {
                    run(canvas, step, op)?;
                    applied.changed(&id);
                }
            }
        }
    }
    Ok(applied)
}

/// A new id for an addition, once its ref is known to be no id of the canvas: `from` and `to`
/// would not know which they name.
fn free_ref(canvas: &Canvas, step: &Step, name: Option<&str>) -> Result<String, ToolError> {
    if let Some(name) = name
        && is_taken(canvas, name)
    {
        return Err(step.fail(
            "invalid",
            format!("the ref {name} is the id of a node or an edge of the canvas: choose another."),
        ));
    }
    Ok(canvas.new_id())
}

fn added(
    applied: &mut Applied,
    refs: &mut Refs,
    step: &Step,
    name: &Option<String>,
    target: Target,
    id: String,
) {
    let key = name.clone().unwrap_or_else(|| {
        // `ops[2] (add_node)` names it as `ops[2]`.
        step.label
            .split_once(' ')
            .map_or(step.label.clone(), |(at, _)| at.into())
    });
    applied.created.insert(key, id.clone().into());
    applied.created_ids.insert(id.clone());
    if let Some(name) = name {
        refs.insert(name.clone(), (target, id));
    }
}

/// Applies an operation made against `canvas` itself: nothing in it can conflict, and what
/// it could not do was checked before; said all the same, rather than written half done.
fn run(canvas: &mut Canvas, step: &Step, op: Op) -> Result<(), ToolError> {
    let mut report = Report::default();
    op.apply(canvas, &mut report);
    if report.is_clean() {
        return Ok(());
    }
    Err(step.fail(
        "invalid",
        format!(
            "{}.",
            report
                .conflicts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ),
    ))
}

fn is_taken(canvas: &Canvas, id: &str) -> bool {
    canvas.node(id).is_some() || canvas.edges().iter().any(|e| e.id() == Some(id))
}

/// The id of the node `name` names (an id, or a ref of this call), in `field`.
fn node_id(
    canvas: &Canvas,
    refs: &Refs,
    step: &Step,
    field: &str,
    name: &str,
) -> Result<String, ToolError> {
    let id = match refs.get(name) {
        Some((Target::Edge, _)) => {
            return Err(step.fail(
                "invalid",
                format!("`{field}` names {name}, an edge added by this call, and takes a node."),
            ));
        }
        Some((Target::Node, id)) => id.as_str(),
        None => name,
    };
    if canvas.node(id).is_some() {
        return Ok(id.into());
    }
    if canvas.edges().iter().any(|e| e.id() == Some(id)) {
        return Err(step.fail(
            "invalid",
            format!("`{field}` names {name}, an edge, and takes a node."),
        ));
    }
    Err(step.fail(
        "not_found",
        format!(
            "`{field}`: no node has the id {name}, nor is it the ref of one this call added before (or it was deleted). Read the canvas again with canvas_read to see its ids."
        ),
    ))
}

/// The id of the edge `name` names (an id, or a ref of this call).
fn edge_id(canvas: &Canvas, refs: &Refs, step: &Step, name: &str) -> Result<String, ToolError> {
    let id = match refs.get(name) {
        Some((Target::Node, _)) => {
            return Err(step.fail(
                "invalid",
                format!("`id` names {name}, a node added by this call, and takes an edge."),
            ));
        }
        Some((Target::Edge, id)) => id.as_str(),
        None => name,
    };
    if canvas.edges().iter().any(|e| e.id() == Some(id)) {
        return Ok(id.into());
    }
    if canvas.node(id).is_some() {
        return Err(step.fail(
            "invalid",
            format!("`id` names {name}, a node, and takes an edge."),
        ));
    }
    Err(step.fail(
        "not_found",
        format!(
            "`id`: no edge has the id {name}, nor is it the ref of one this call added before (or it was deleted). Read the canvas again with canvas_read to see its ids."
        ),
    ))
}

/// What a node is, as its `type` says.
fn kind_name(node: &Node) -> &str {
    match node.kind() {
        Some(NodeKind::Text) => "text",
        Some(NodeKind::File) => "file",
        Some(NodeKind::Link) => "link",
        Some(NodeKind::Group) => "group",
        Some(NodeKind::Other(other)) => other,
        None => "untyped",
    }
}

/// How many times `old` is in `text`, overlaps counted, up to two: once is all that matters.
fn occurrences(text: &str, old: &str) -> usize {
    let mut count = 0;
    let mut from = 0;
    while let Some(at) = text[from..].find(old) {
        count += 1;
        if count == 2 {
            break;
        }
        from += at + old.chars().next().map_or(1, char::len_utf8);
    }
    count
}

const OUTPUT: &str = r#"{
    "type": "object",
    "properties": {
        "revision": {"type": "string", "description": "A hash of the file's text, once written."},
        "created": {
            "type": "object",
            "additionalProperties": {"type": "string"},
            "description": "Each ref given, or for an addition without one its place (\"ops[2]\", \"nodes[0]\"), with the id of the node or edge added."
        },
        "changed": {"type": "array", "items": {"type": "string"}, "description": "The ids of the nodes and edges changed."},
        "deleted": {"type": "array", "items": {"type": "string"}, "description": "The ids of the nodes and edges deleted, the edges of a node deleted included."}
    },
    "required": ["revision", "created", "changed", "deleted"]
}"#;

fn output_schema() -> Value {
    serde_json::from_str(OUTPUT).expect("the output schema is JSON")
}

/// The properties of an operation, `op` aside: an add_node's for canvas_create's `nodes`, an
/// add_edge's for its `edges`, all of them for canvas_edit's `ops`.
fn op_properties(fields: &[&str]) -> Map<String, Value> {
    let all = json!({
        "id": {"type": "string", "description": "update_*, delete_*, to_front, to_back: the node or edge, by its id or the ref of one added before in this call."},
        "ref": {"type": "string", "description": "add_*: a name of your choosing (letters, digits, _ - . :) for what is added, by which the operations after it name it; `created` gives its id."},
        "type": {"type": "string", "enum": ["text", "file", "link", "group"], "description": "add_node: the card's type."},
        "text": {"type": "string", "description": "A text card's Markdown."},
        "replace": {
            "type": "object",
            "properties": {"old": {"type": "string"}, "new": {"type": "string"}},
            "required": ["old", "new"],
            "additionalProperties": false,
            "description": "update_node: changes the words `old` of a text card into `new`, only if `old` is in its text once. The safe way to correct a card the user may be editing."
        },
        "file": {"type": "string", "description": "A file card's file, relative to the vault."},
        "subpath": {"type": ["string", "null"], "description": "A file card's heading or block, beginning with #."},
        "url": {"type": "string", "description": "A link card's address."},
        "label": {"type": ["string", "null"], "description": "A group's label, or an edge's."},
        "x": {"type": "number", "description": "The card's left, in canvas pixels. add_node without x and y: Rature places the card where it overlaps none."},
        "y": {"type": "number", "description": "The card's top; y grows downwards."},
        "width": {"type": "number", "description": "add_node without it: a width that fits the text."},
        "height": {"type": "number", "description": "add_node without it: a height that fits the text."},
        "color": {"type": ["string", "null"], "description": format!("{COLOR}; null removes it.")},
        "near": {"type": "string", "description": "add_node without x and y: the node (id or ref) to put the card right of, rather than right of all the cards."},
        "from": {"type": "string", "description": "An edge's start: a node's id or ref."},
        "to": {"type": "string", "description": "An edge's end: a node's id or ref."},
        "fromSide": {"type": ["string", "null"], "enum": ["top", "right", "bottom", "left", null]},
        "toSide": {"type": ["string", "null"], "enum": ["top", "right", "bottom", "left", null]},
        "fromEnd": {"type": ["string", "null"], "enum": ["none", "arrow", null], "description": "none unless said."},
        "toEnd": {"type": ["string", "null"], "enum": ["none", "arrow", null], "description": "arrow unless said."},
    });
    let Value::Object(all) = all else {
        unreachable!("an object")
    };
    all.into_iter()
        .filter(|(key, _)| fields.contains(&key.as_str()))
        .collect()
}

pub(crate) fn canvas_edit_tool() -> Value {
    let mut fields: Vec<&str> = ADD_NODE.into();
    fields.extend(UPDATE_NODE);
    fields.extend(ADD_EDGE);
    let mut properties = op_properties(&fields);
    properties.shift_insert(
        0,
        "op".into(),
        json!({"type": "string", "enum": OPS, "description": "What the operation does."}),
    );
    json!({
        "name": "canvas_edit",
        "title": "Change a canvas",
        "description": "Changes a canvas by operations on ids, applied in order, all or none, in one write. Each touches only the fields it names, on the file as it is when written: what the user changed elsewhere meanwhile stays. Operations, each an object with `op`:\n- add_node {type, text | file, subpath? | url | label?, x?, y?, width?, height?, color?, near?, ref?}: without x and y, Rature places the card right of `near` (or of all the cards) where it overlaps none; without a size, one that fits its text. Prefer that to placing cards yourself.\n- update_node {id, x?, y?, width?, height?, color?, text? | replace?: {old, new}, label?, url?, file?, subpath?}: to correct words, prefer `replace` to `text`.\n- delete_node {id}: its edges go with it.\n- add_edge {from, to, fromSide?, toSide?, fromEnd?, toEnd?, label?, color?, ref?}; update_edge {id, from?, to?, fromSide?, toSide?, fromEnd?, toEnd?, label?, color?}; delete_edge {id}.\n- to_front {id}, to_back {id}: a node drawn above, or below, all the others.\n`ref` names what an operation adds, for the operations after it (in `id`, `from`, `to`, `near`). null removes an optional field (color, label, subpath, sides, ends). With `if_revision`, the call is refused if the file changed since that revision.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": mcp::path_schema("The .canvas file"),
                "ops": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": properties,
                        "required": ["op"],
                        "additionalProperties": false,
                    },
                    "description": "The operations, applied in order: all of them, or none.",
                },
                "if_revision": {
                    "type": "string",
                    "description": "The `revision` canvas_read gave: the call is refused, as stale, if the file changed since. Left out, the operations are replayed by id on the file as it is.",
                },
            },
            "required": ["path", "ops"],
            "additionalProperties": false,
        },
        "outputSchema": output_schema(),
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

pub(crate) fn canvas_create_tool() -> Value {
    let entry = |fields: &[&str], required: &[&str]| {
        json!({
            "type": "object",
            "properties": op_properties(fields),
            "required": required,
            "additionalProperties": false,
        })
    };
    json!({
        "name": "canvas_create",
        "title": "Make a canvas",
        "description": "Makes a new canvas file, with cards and edges. Refused if the file exists (change it with canvas_edit), or if its folder does not (no folder is made). `nodes` are add_node operations and `edges` add_edge ones, as canvas_edit takes them, without `op`; an edge names its nodes by their `ref`. Cards without x and y are placed by Rature where they overlap none, each right of `near` or of all the cards before it; without a size, one that fits its text.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": mcp::path_schema("The new .canvas file"),
                "nodes": {
                    "type": "array",
                    "items": entry(&ADD_NODE, &["type"]),
                    "description": "The cards, the first drawn below the others.",
                },
                "edges": {
                    "type": "array",
                    "items": entry(&ADD_EDGE, &["from", "to"]),
                    "description": "The edges between the cards, named by their `ref`.",
                },
            },
            "required": ["path"],
            "additionalProperties": false,
        },
        "outputSchema": output_schema(),
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrences_count_overlaps_up_to_two() {
        assert_eq!(occurrences("abc", "x"), 0);
        assert_eq!(occurrences("abc", "b"), 1);
        assert_eq!(occurrences("aaa", "aa"), 2);
        assert_eq!(occurrences("éé é", "é é"), 1);
        assert_eq!(occurrences("a b a b a", "a"), 2);
    }

    #[test]
    fn the_schemas_are_json() {
        for tool in [canvas_edit_tool(), canvas_create_tool()] {
            assert_eq!(
                tool["outputSchema"]["required"].as_array().unwrap().len(),
                4
            );
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let ops = &canvas_edit_tool()["inputSchema"]["properties"]["ops"]["items"]["properties"];
        assert_eq!(ops.as_object().unwrap().keys().next().unwrap(), "op");
        for field in ADD_NODE.iter().chain(&UPDATE_NODE).chain(&UPDATE_EDGE) {
            assert!(ops.get(*field).is_some(), "{field}");
        }
    }
}
