//! `rature mcp`: an MCP server on stdin and stdout, for a program that lays out canvases the
//! user then reads in Rature. It works on the files alone: an open window sees each change
//! through its watcher. No window, no display, no channel to a window.
//!
//! Written by hand, synchronous: one JSON-RPC message a line on stdin, one a line on stdout,
//! which carries nothing else (what is told to a person goes to stderr). It speaks both eras of
//! MCP (<https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning>):
//!
//! - the modern one, `2026-07-28`, stateless: each request carries its protocol version and the
//!   client's capabilities in `_meta`, `server/discover` says what the server is, and each result
//!   has a `resultType` and the server's identity in its `_meta`;
//! - the legacy one, `2025-11-25` (and `2025-06-18`), opened by `initialize`: the requests after
//!   it that carry no modern `_meta` are served in the version it agreed.
//!
//! A request with the modern `_meta` is served as a modern one even after `initialize`: the
//! specification lets a server serve both eras in one process.
//!
//! The server reads only inside its allowed folders: the folder it starts in and each
//! `--allow-dir`, canonical, compared component by component; a path is canonicalised (its
//! links followed) before it is compared, and the file opened is checked again.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::canvas::{Canvas, Problem};

pub const USAGE: &str = "usage: rature mcp [--allow-dir DIR]...

Serves MCP on stdin and stdout (one JSON-RPC message a line; logs on stderr), with the tool
canvas_read. It reads only in the folder it starts in and in each DIR given.";

/// The modern version, served without a handshake.
const MODERN: &str = "2026-07-28";
/// The versions `initialize` agrees to, the latest first: the one given when the client asks
/// for another.
const LEGACY: [&str; 2] = ["2025-11-25", "2025-06-18"];

const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// How long a client may keep `server/discover`'s and `tools/list`'s answers: they change only
/// with the binary, which a client starts again.
const TTL_MS: u64 = 3_600_000;

/// Past this many bytes of JSON, a `canvas_read` that did not ask for `text` gets a summary,
/// marked `truncated`: a client keeps a tool's answer to some 25 000 tokens (Claude Code).
const SUMMARY_PAST: usize = 50_000;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_VERSION: i64 = -32022;

/// What `initialize` and `server/discover` tell the model, the order of the calls first.
const INSTRUCTIONS: &str = "Rature shows JSON Canvas files (.canvas) to the user: boards of cards (nodes of type text, file, link or group) joined by edges. The file is the only truth, and the user may be changing it in Rature meanwhile.
- Read a canvas with canvas_read before working on it. Each node and edge has an `id`: name them by it, never by their place in the list.
- The order of `nodes` is the drawing order: the first is drawn below the others.
- `revision` is a hash of the file's text: another one means the file changed since.
- A canvas too large to be given whole comes as a summary (each card's first line) with `truncated: true`: then read the cards you need with `ids`.
- A canvas with flaws (a duplicate id, an edge to a missing node) comes with `problems`.
- A path is relative to the folder the server started in, or absolute; it must be inside the folders the server may read.
- An error ends with its code in brackets ([not_found], [invalid], [not_a_canvas], [outside], [io]) and says what to do.";

/// Runs the server on the arguments after `mcp`, until stdin ends; the process's exit code.
pub fn main(args: impl Iterator<Item = OsString>) -> i32 {
    let mut asked = Vec::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return 0;
            }
            Some("--allow-dir") => match args.next() {
                Some(dir) => asked.push(PathBuf::from(dir)),
                None => return usage("--allow-dir needs a folder"),
            },
            Some(arg) if arg.starts_with("--allow-dir=") => {
                asked.push(PathBuf::from(&arg["--allow-dir=".len()..]));
            }
            _ => return usage(&format!("unknown argument {}", Path::new(&arg).display())),
        }
    }
    let folders = match std::env::current_dir().and_then(|start| Folders::new(&start, &asked)) {
        Ok(folders) => folders,
        Err(error) => {
            eprintln!("rature mcp: {error}");
            return 1;
        }
    };
    eprintln!(
        "rature mcp: reading in {}",
        folders
            .allowed
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut server = Server {
        folders,
        legacy: None,
    };
    match server.serve(io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("rature mcp: {error}");
            1
        }
    }
}

fn usage(what: &str) -> i32 {
    eprintln!("rature mcp: {what}\n{USAGE}");
    2
}

/// The folders the server reads in, canonical.
struct Folders {
    /// Where a relative path starts: the folder the server started in.
    start: PathBuf,
    /// `start`, then each `--allow-dir`, each once.
    allowed: Vec<PathBuf>,
}

impl Folders {
    fn new(start: &Path, asked: &[PathBuf]) -> io::Result<Self> {
        let start = fs::canonicalize(start).map_err(|error| {
            io::Error::new(error.kind(), format!("the current folder: {error}"))
        })?;
        let mut allowed = vec![start.clone()];
        for dir in asked {
            let real = fs::canonicalize(dir).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("--allow-dir {}: {error}", dir.display()),
                )
            })?;
            if !real.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("--allow-dir {}: not a folder", dir.display()),
                ));
            }
            if !allowed.contains(&real) {
                allowed.push(real);
            }
        }
        Ok(Self { start, allowed })
    }

    /// Whether a canonical path is in one of the folders: compared component by component, so
    /// that `/a/bc` is not in `/a/b`.
    fn holds(&self, real: &Path) -> bool {
        self.allowed.iter().any(|dir| real.starts_with(dir))
    }

    fn check(&self, real: &Path, asked: &str) -> Result<(), ToolError> {
        if self.holds(real) {
            return Ok(());
        }
        Err(ToolError::new(
            "outside",
            format!(
                "{asked}{} is outside the folders this server may read: {}. Give a path inside one of them, or start `rature mcp` again with `--allow-dir DIR` to add one.",
                leads(asked, real),
                self.list()
            ),
        ))
    }

    fn list(&self) -> String {
        self.allowed
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The canonical path of `asked` (relative to `start`, or absolute), once it is known to be
    /// inside the folders. A path that does not exist is placed by the deepest folder above it
    /// that does: outside, it is said to be; inside, the system's error is.
    fn resolve(&self, asked: &str) -> Result<PathBuf, ToolError> {
        let path = self.start.join(asked);
        match fs::canonicalize(&path) {
            Ok(real) => {
                self.check(&real, asked)?;
                Ok(real)
            }
            Err(error) => {
                if let Some(real) = path
                    .ancestors()
                    .skip(1)
                    .find_map(|a| fs::canonicalize(a).ok())
                {
                    self.check(&real, asked)?;
                }
                Err(ToolError::io(asked, &error))
            }
        }
    }

    /// The text of the canvas file at `asked`, read only when it is a regular `.canvas` file
    /// inside the folders.
    fn read_canvas(&self, asked: &str) -> Result<String, ToolError> {
        let real = self.resolve(asked)?;
        if real.extension().and_then(|e| e.to_str()) != Some("canvas") {
            return Err(ToolError::new(
                "invalid",
                format!(
                    "`path` must name a .canvas file, and {asked} does not{}.",
                    leads(asked, &real)
                ),
            ));
        }
        // Checked before it is opened: opening a named pipe waits for a writer.
        let meta = fs::metadata(&real).map_err(|error| ToolError::io(asked, &error))?;
        if !meta.is_file() {
            return Err(not_a_file(asked, &meta));
        }
        let mut file = File::open(&real).map_err(|error| ToolError::io(asked, &error))?;
        // What was opened, rather than what the path led to a moment before: a folder on the
        // way may have been swapped for a link since.
        if let Ok(opened) = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())) {
            self.check(&opened, asked)?;
        }
        let meta = file
            .metadata()
            .map_err(|error| ToolError::io(asked, &error))?;
        if !meta.is_file() {
            return Err(not_a_file(asked, &meta));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| ToolError::io(asked, &error))?;
        String::from_utf8(bytes).map_err(|error| {
            ToolError::new(
                "not_a_canvas",
                format!(
                    "{asked} is not a canvas: it is not UTF-8 text (at byte {}). Read another file.",
                    error.utf8_error().valid_up_to()
                ),
            )
        })
    }
}

fn leads(asked: &str, real: &Path) -> String {
    if Path::new(asked) == real {
        String::new()
    } else {
        format!(" (it leads to {})", real.display())
    }
}

fn not_a_file(asked: &str, meta: &fs::Metadata) -> ToolError {
    let what = if meta.is_dir() {
        "a folder"
    } else {
        "not a regular file"
    };
    ToolError::new(
        "invalid",
        format!("`path` must name a .canvas file, and {asked} is {what}."),
    )
}

/// A tool's failure: one sentence saying what to do, and its code.
#[derive(Debug)]
struct ToolError {
    code: &'static str,
    message: String,
}

impl ToolError {
    fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }

    fn invalid(message: String) -> Self {
        Self::new("invalid", message)
    }

    fn io(asked: &str, error: &io::Error) -> Self {
        let hint = match error.kind() {
            io::ErrorKind::NotFound => {
                " Check the path: it is relative to the folder the server started in, or absolute."
            }
            _ => "",
        };
        Self::new("io", format!("{asked} cannot be read: {error}.{hint}"))
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.message, self.code)
    }
}

/// A JSON-RPC error.
struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

struct Server {
    folders: Folders,
    /// The version `initialize` agreed, once it came.
    legacy: Option<&'static str>,
}

impl Server {
    fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                return Ok(());
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Some(answer) = self.answer(&line) {
                let mut text = serde_json::to_string(&answer).expect("a JSON value is written");
                text.push('\n');
                output.write_all(text.as_bytes())?;
                output.flush()?;
            }
        }
    }

    /// The answer to one line: none for a notification, nor for a response.
    fn answer(&mut self, line: &[u8]) -> Option<Value> {
        let message = match std::str::from_utf8(line)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str::<Value>(text).map_err(|e| e.to_string()))
        {
            Ok(message) => message,
            Err(error) => {
                return Some(failure(
                    Value::Null,
                    RpcError::new(PARSE_ERROR, format!("Parse error: {error}")),
                ));
            }
        };
        let Value::Object(message) = message else {
            return Some(failure(
                Value::Null,
                RpcError::new(
                    INVALID_REQUEST,
                    "Invalid request: a message is one JSON object (MCP has no batches)",
                ),
            ));
        };
        let id = message.get("id");
        if !message.contains_key("method") {
            // A response: the server asks nothing, so it answers none.
            if message.contains_key("result") || message.contains_key("error") {
                return None;
            }
            return Some(failure(
                id.filter(|id| is_id(id)).cloned().unwrap_or(Value::Null),
                RpcError::new(INVALID_REQUEST, "Invalid request: no `method`"),
            ));
        }
        // A notification (`notifications/initialized`, `notifications/cancelled`...): nothing
        // to do, as every request is answered before the next line is read.
        let id = id?;
        if !is_id(id) {
            return Some(failure(
                Value::Null,
                RpcError::new(
                    INVALID_REQUEST,
                    "Invalid request: `id` must be a string or an integer",
                ),
            ));
        }
        let id = id.clone();
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(failure(
                id,
                RpcError::new(
                    INVALID_REQUEST,
                    "Invalid request: `jsonrpc` must be \"2.0\"",
                ),
            ));
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Some(failure(
                id,
                RpcError::new(
                    INVALID_REQUEST,
                    "Invalid request: `method` must be a string",
                ),
            ));
        };
        let empty = Map::new();
        let params = match message.get("params") {
            None => &empty,
            Some(Value::Object(params)) => params,
            Some(_) => {
                return Some(failure(
                    id,
                    RpcError::new(INVALID_PARAMS, "Invalid params: `params` must be an object"),
                ));
            }
        };
        Some(match self.request(method, params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => failure(id, error),
        })
    }

    fn request(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError> {
        if method == "initialize" {
            return Ok(self.initialize(params));
        }
        let meta = params.get("_meta").and_then(Value::as_object);
        match meta.and_then(|meta| Some((meta, meta.get(PROTOCOL_VERSION)?))) {
            Some((meta, version)) => modern(&self.folders, method, params, meta, version),
            None if self.legacy.is_some() => self.serve_legacy(method, params),
            // Before `initialize` too, in the legacy era.
            None if method == "ping" => Ok(json!({})),
            None => Err(RpcError::new(
                INVALID_PARAMS,
                format!(
                    "Invalid params: `_meta` has no `{PROTOCOL_VERSION}`: give it on each request ({MODERN}), or begin with `initialize` ({})",
                    LEGACY[0]
                ),
            )),
        }
    }

    fn initialize(&mut self, params: &Map<String, Value>) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = LEGACY
            .into_iter()
            .find(|v| Some(*v) == asked)
            .unwrap_or(LEGACY[0]);
        self.legacy = Some(version);
        json!({
            "protocolVersion": version,
            "capabilities": capabilities(),
            "serverInfo": server_info(),
            "instructions": INSTRUCTIONS,
        })
    }

    fn serve_legacy(&self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError> {
        match method {
            "ping" => Ok(json!({})),
            "tools/list" => tools_list(params).map(Value::Object),
            "tools/call" => tools_call(&self.folders, params),
            _ => Err(unknown_method(method)),
        }
    }
}

/// A request of the modern era, its `_meta` checked first.
fn modern(
    folders: &Folders,
    method: &str,
    params: &Map<String, Value>,
    meta: &Map<String, Value>,
    version: &Value,
) -> Result<Value, RpcError> {
    let Some(version) = version.as_str() else {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("Invalid params: `_meta`'s `{PROTOCOL_VERSION}` must be a string"),
        ));
    };
    if !meta.get(CLIENT_CAPABILITIES).is_some_and(Value::is_object) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("Invalid params: `_meta` has no `{CLIENT_CAPABILITIES}` object"),
        ));
    }
    if version != MODERN {
        return Err(RpcError {
            code: UNSUPPORTED_VERSION,
            message: "Unsupported protocol version".into(),
            data: Some(json!({"supported": supported(), "requested": version})),
        });
    }
    let mut result = match method {
        "server/discover" => {
            let mut result = Map::new();
            result.insert("supportedVersions".into(), json!(supported()));
            result.insert("capabilities".into(), capabilities());
            result.insert("instructions".into(), INSTRUCTIONS.into());
            cacheable(&mut result);
            result
        }
        "tools/list" => {
            let mut result = tools_list(params)?;
            cacheable(&mut result);
            result
        }
        "tools/call" => match tools_call(folders, params)? {
            Value::Object(result) => result,
            _ => unreachable!("a tool's result is an object"),
        },
        _ => return Err(unknown_method(method)),
    };
    result.insert("resultType".into(), "complete".into());
    result.insert("_meta".into(), json!({ SERVER_INFO: server_info() }));
    Ok(Value::Object(result))
}

fn supported() -> Vec<&'static str> {
    std::iter::once(MODERN).chain(LEGACY).collect()
}

fn cacheable(result: &mut Map<String, Value>) {
    result.insert("ttlMs".into(), TTL_MS.into());
    result.insert("cacheScope".into(), "public".into());
}

fn capabilities() -> Value {
    json!({"tools": {}})
}

fn server_info() -> Value {
    json!({
        "name": "rature",
        "title": "Rature",
        "version": env!("RATURE_VERSION_NUMBER"),
    })
}

fn unknown_method(method: &str) -> RpcError {
    RpcError::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
}

fn is_id(id: &Value) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}

fn failure(id: Value, error: RpcError) -> Value {
    let mut body = json!({"code": error.code, "message": error.message});
    if let Some(data) = error.data {
        body["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": body})
}

fn tools_list(params: &Map<String, Value>) -> Result<Map<String, Value>, RpcError> {
    // The list is one page: no cursor is ever given out.
    if params.get("cursor").is_some_and(|cursor| !cursor.is_null()) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "Invalid params: unknown cursor (the list has one page)",
        ));
    }
    let mut result = Map::new();
    result.insert("tools".into(), json!([canvas_read_tool()]));
    Ok(result)
}

fn canvas_read_tool() -> Value {
    json!({
        "name": "canvas_read",
        "title": "Read a canvas",
        "description": "Reads a JSON Canvas file (.canvas): its nodes (cards) and its edges, in the file's order (the first node is drawn below the others), each with all its fields, unknown ones included. `revision` is a hash of the file's text: it changes with any change to the file. `text: \"summary\"` cuts each card's text to its first line; without `text`, a canvas too large to be given whole (over 50 kB of JSON) is given so, with `truncated: true`. `ids` keeps only the nodes and edges named. A canvas with flaws (a duplicate id, an edge to a missing node) is given with `problems`, one sentence each.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The .canvas file: relative to the folder the server started in, or absolute. It must be inside the folders the server may read.",
                },
                "ids": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Only the nodes and edges with these ids; all of them when left out.",
                },
                "text": {
                    "type": "string",
                    "enum": ["full", "summary"],
                    "default": "full",
                    "description": "\"full\": each card's text whole, however large the canvas. \"summary\": its first line only. Left out: whole, unless the canvas is too large.",
                },
            },
            "required": ["path"],
            "additionalProperties": false,
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "revision": {"type": "string", "description": "A hash of the file's text."},
                // No field is required of a node or an edge: a canvas with flaws is read too.
                "nodes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"id": {"type": "string"}, "type": {"type": "string"}},
                    },
                },
                "edges": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "fromNode": {"type": "string"},
                            "toNode": {"type": "string"},
                        },
                    },
                },
                "problems": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "What is wrong with the canvas, one sentence each; such a canvas cannot be changed until it is mended.",
                },
                "truncated": {
                    "type": "boolean",
                    "description": "The canvas was too large: each card's text is cut to its first line.",
                },
                "note": {"type": "string", "description": "What to do about `truncated`."},
            },
            "required": ["revision", "nodes", "edges"],
        },
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

/// `tools/call`: an unknown tool or a malformed request is a JSON-RPC error; a tool's failure
/// is a result with `isError`, which the model reads.
fn tools_call(folders: &Folders, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "Invalid params: `name` must be a tool's name",
        ));
    };
    let empty = Map::new();
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(arguments)) => arguments,
        Some(_) => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "Invalid params: `arguments` must be an object",
            ));
        }
    };
    let outcome = match name {
        "canvas_read" => canvas_read(folders, arguments),
        _ => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("Unknown tool: {name}"),
            ));
        }
    };
    Ok(match outcome {
        Ok(output) => json!({
            "content": [{"type": "text", "text": serde_json::to_string(&output).expect("a JSON value is written")}],
            "structuredContent": output,
            "isError": false,
        }),
        Err(error) => json!({
            "content": [{"type": "text", "text": error.to_string()}],
            "isError": true,
        }),
    })
}

fn canvas_read(folders: &Folders, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !["path", "ids", "text"].contains(&key.as_str()))
    {
        return Err(ToolError::invalid(format!(
            "`{key}` is not an argument of canvas_read, which takes `path`, `ids` and `text`."
        )));
    }
    let path = match arguments.get("path") {
        Some(Value::String(path)) if !path.is_empty() => path.as_str(),
        None | Some(Value::Null | Value::String(_)) => {
            return Err(ToolError::invalid(format!(
                "`path` is required: the .canvas file, relative to {} or absolute.",
                folders.start.display()
            )));
        }
        Some(other) => {
            return Err(ToolError::invalid(format!(
                "`path` must be a string, not {other}."
            )));
        }
    };
    let ids: Option<Vec<&str>> = match arguments.get("ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(ids)) => Some(
            ids.iter()
                .map(Value::as_str)
                .collect::<Option<_>>()
                .ok_or_else(|| {
                    ToolError::invalid(
                        "`ids` must be a list of strings: ids of nodes or edges.".into(),
                    )
                })?,
        ),
        Some(_) => {
            return Err(ToolError::invalid(
                "`ids` must be a list of strings: ids of nodes or edges.".into(),
            ));
        }
    };
    // `None`: not asked, and then a summary past `SUMMARY_PAST`.
    let summary = match arguments.get("text") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text == "full" => Some(false),
        Some(Value::String(text)) if text == "summary" => Some(true),
        Some(other) => {
            return Err(ToolError::invalid(format!(
                "`text` must be \"full\" or \"summary\", not {other}."
            )));
        }
    };

    let text = folders.read_canvas(path)?;
    let canvas = Canvas::read(&text);
    let problems = canvas.problems();
    // Only what is not JSON cannot be read at all; the rest is read as far as it can be, and
    // said (writing it is refused, #18).
    if let Some(Problem::Json(error)) = problems.iter().find(|p| matches!(p, Problem::Json(_))) {
        return Err(ToolError::new(
            "not_a_canvas",
            format!(
                "{path} is not a canvas: it is not JSON ({error}). Mend the file, or read another."
            ),
        ));
    }
    let wanted: Option<HashSet<&str>> = ids.map(|ids| ids.into_iter().collect());
    if let Some(wanted) = &wanted {
        let known: HashSet<&str> = canvas
            .nodes()
            .iter()
            .filter_map(|node| node.id())
            .chain(canvas.edges().iter().filter_map(|edge| edge.id()))
            .collect();
        let mut unknown: Vec<&str> = wanted.difference(&known).copied().collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(ToolError::new(
                "not_found",
                format!(
                    "No node or edge of {path} has the id {}: read the canvas again without `ids` to see the ids it has now.",
                    unknown.join(", ")
                ),
            ));
        }
    }
    let kept = |id: Option<&str>| {
        wanted
            .as_ref()
            .is_none_or(|wanted| id.is_some_and(|id| wanted.contains(id)))
    };
    let nodes = |summary: bool| -> Vec<Value> {
        canvas
            .nodes()
            .iter()
            .filter(|node| kept(node.id()))
            .map(|node| {
                let mut fields = node.fields().clone();
                if summary && let Some(Value::String(text)) = fields.get_mut("text") {
                    first_line(text);
                }
                Value::Object(fields)
            })
            .collect()
    };
    let edges: Vec<Value> = canvas
        .edges()
        .iter()
        .filter(|edge| kept(edge.id()))
        .map(|edge| Value::Object(edge.fields().clone()))
        .collect();
    let mut output = Map::new();
    output.insert("revision".into(), revision(&text).into());
    output.insert("nodes".into(), nodes(summary == Some(true)).into());
    output.insert("edges".into(), edges.into());
    if !problems.is_empty() {
        let problems: Vec<String> = problems
            .iter()
            .map(|p| format!("{}.", capital(&p.to_string())))
            .collect();
        output.insert("problems".into(), problems.into());
    }
    if summary.is_none() && json_len(&output) > SUMMARY_PAST {
        output.insert("nodes".into(), nodes(true).into());
        output.insert("truncated".into(), true.into());
        output.insert(
            "note".into(),
            format!(
                "The canvas is too large to be given whole (over {} kB): each card's text is cut to its first line. Read the cards you need again with `ids`, or the whole canvas with `text: \"full\"`.",
                SUMMARY_PAST / 1000
            )
            .into(),
        );
    }
    Ok(Value::Object(output))
}

fn json_len(value: &Map<String, Value>) -> usize {
    serde_json::to_string(value).map_or(0, |text| text.len())
}

/// A sentence's first letter in capital.
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Cuts a text to its first line.
fn first_line(text: &mut String) {
    if let Some(end) = text.find('\n') {
        let end = if text[..end].ends_with('\r') {
            end - 1
        } else {
            end
        };
        text.truncate(end);
    }
}

/// A hash of a file's text, the same from one process and one build to the next: FNV-1a on
/// 128 bits, in 32 hexadecimal digits. It tells one text from another, no more: it is no
/// protection against a text made to collide.
pub fn revision(text: &str) -> String {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013B;
    let hash = text.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(byte)).wrapping_mul(PRIME)
    });
    format!("{hash:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_revision_is_fnv1a_128() {
        // The offset basis, for no byte; and the published value for "a".
        assert_eq!(revision(""), "6c62272e07bb014262b821756295c58d");
        assert_eq!(revision("a"), "d228cb696f1a8caf78912b704e4a8964");
        assert_ne!(revision("{}"), revision("{ }"));
    }

    #[test]
    fn a_folder_holds_its_own_paths_only() {
        let folders = Folders {
            start: "/a/b".into(),
            allowed: vec!["/a/b".into(), "/c".into()],
        };
        assert!(folders.holds(Path::new("/a/b")));
        assert!(folders.holds(Path::new("/a/b/x.canvas")));
        assert!(folders.holds(Path::new("/c/d/e.canvas")));
        assert!(!folders.holds(Path::new("/a/bc/x.canvas")));
        assert!(!folders.holds(Path::new("/a")));
        assert!(!folders.holds(Path::new("/cd")));
    }

    #[test]
    fn a_summary_keeps_the_first_line() {
        for (text, first) in [("a\nb", "a"), ("a\r\nb", "a"), ("a", "a"), ("\nb", "")] {
            let mut text = text.to_owned();
            first_line(&mut text);
            assert_eq!(text, first);
        }
    }
}
