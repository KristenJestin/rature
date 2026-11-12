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
//! The server reads and writes only inside its allowed folders: the folder it starts in and each
//! `--allow-dir`, canonical, compared component by component; a path is canonicalised (its
//! links followed) before it is compared, and the file opened is checked again. It writes only
//! `.canvas` files, by the tools of `mcp_edit`, through their folder opened and checked again,
//! so that a folder swapped for a link meanwhile leads nowhere else.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::canvas::{Canvas, Problem, revision};
use crate::fsio;
use crate::mcp_edit;

pub const USAGE: &str = "usage: rature mcp [--allow-dir DIR]...

Serves MCP on stdin and stdout (one JSON-RPC message a line; logs on stderr), with the tools
canvas_read, canvas_edit and canvas_create. It reads and writes only in the folder it starts
in and in each DIR given, and writes only .canvas files.";

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

/// The longest line read on stdin, its end included: past it, the line is skipped without being
/// kept, and answered with a parse error. A request is far smaller: a `canvas_create` of a few
/// thousand cards of text is some hundreds of kilobytes; this leaves room for any canvas a
/// person reads, and keeps a line that never ends from filling the memory.
const LONGEST_LINE: usize = 16 * 1024 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_VERSION: i64 = -32022;

/// What `initialize` and `server/discover` tell the model, the order of the calls first.
const INSTRUCTIONS: &str = "Rature shows JSON Canvas files (.canvas) to the user: boards of cards (nodes of type text, file, link or group) joined by edges. The file is the only truth, and the user may be changing it in Rature meanwhile.
- Read a canvas with canvas_read before working on it. Each node and edge has an `id`: name them by it, never by their place in the list.
- Change it with canvas_edit: operations by id, applied in order, all or none. Each touches only the fields it names, replayed on the file as it is when it is written, so what the user changed elsewhere stays. Give `if_revision` (canvas_read's `revision`) to be refused if the file changed since.
- To correct words in a card, prefer `replace: {old, new}` to a whole `text`: it changes them only if they are in the text once, and keeps what the user typed around them.
- Let Rature place new cards: leave out x and y (and the size), with `near` to put a card beside another; Rature puts it where it overlaps no card.
- Make a new canvas with canvas_create; it never writes over a file.
- The order of `nodes` is the drawing order: the first is drawn below the others.
- `revision` is a hash of the file's text: another one means the file changed since.
- A canvas too large to be given whole comes as a summary (each card's first line) with `truncated: true`: then read the cards you need with `ids`.
- A canvas with flaws (a duplicate id, an edge to a missing node) comes with `problems`, and cannot be changed until it is mended.
- A path is relative to the folder the server started in, or absolute; it must be inside the folders the server may use.
- An error ends with its code in brackets ([not_found], [invalid], [not_a_canvas], [outside], [io], [stale], [busy], [exists]) and says what to do.";

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
        "rature mcp: reading and writing in {}",
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

/// The folders the server reads and writes in, canonical.
pub(crate) struct Folders {
    /// Where a relative path starts: the folder the server started in.
    pub(crate) start: PathBuf,
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
                "{asked}{} is outside the folders this server may use: {}. Give a path inside one of them, or start `rature mcp` again with `--allow-dir DIR` to add one.",
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
        self.resolve_path(&self.start.join(asked), asked)
    }

    /// [`Folders::resolve`] of `path`, which `asked` names.
    fn resolve_path(&self, path: &Path, asked: &str) -> Result<PathBuf, ToolError> {
        match fs::canonicalize(path) {
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

    /// The canonical path of the canvas file at `asked`, when it is a regular `.canvas` file
    /// inside the folders: the only kind of file the server reads as a canvas, or writes.
    pub(crate) fn canvas_file(&self, asked: &str) -> Result<PathBuf, ToolError> {
        let real = self.resolve(asked)?;
        if !is_canvas(&real) {
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
        Ok(real)
    }

    /// The canonical path a new canvas at `asked` would have: a `.canvas` name in a folder that
    /// exists inside the folders (none is made), where nothing is yet.
    pub(crate) fn new_canvas_file(&self, asked: &str) -> Result<PathBuf, ToolError> {
        let path = self.start.join(asked);
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(ToolError::invalid(format!(
                "`path` must name a .canvas file, and {asked} does not."
            )));
        };
        if !is_canvas(Path::new(name)) {
            return Err(ToolError::invalid(format!(
                "`path` must name a .canvas file, and {asked} does not."
            )));
        }
        let folder = self.resolve_path(folder, asked).map_err(|error| {
            if error.code != "io" {
                return error;
            }
            ToolError::new(
                "io",
                format!(
                    "The folder of {asked}, {}, cannot be used: {}. canvas_create makes no folder: give a path in a folder that exists.",
                    folder.display(),
                    error.cause
                ),
            )
        })?;
        if !folder.is_dir() {
            return Err(ToolError::invalid(format!(
                "`path` must name a .canvas file in a folder, and {} is not a folder.",
                folder.display()
            )));
        }
        let real = folder.join(name);
        // A link is something there too, even one to nothing.
        if fs::symlink_metadata(&real).is_ok() {
            return Err(exists(asked));
        }
        Ok(real)
    }

    /// The folder of `real` (a canonical path inside the folders, which `asked` names) opened,
    /// and `real`'s name in it: what a write goes through, so that a folder on the way swapped
    /// for a link once it is opened leads nowhere else. What was opened is checked again.
    pub(crate) fn open_folder<'a>(
        &self,
        real: &'a Path,
        asked: &str,
    ) -> Result<(File, &'a OsStr), ToolError> {
        let (Some(folder), Some(name)) = (real.parent(), real.file_name()) else {
            return Err(ToolError::invalid(format!(
                "`path` must name a .canvas file, and {asked} does not."
            )));
        };
        let cannot = |error: io::Error| ToolError::io_doing(asked, "written", &error);
        let opened = File::open(folder).map_err(cannot)?;
        // What was opened, rather than what the path led to a moment before: a folder on the
        // way may have been swapped for a link since.
        self.check(&fsio::folder_path(&opened).map_err(cannot)?, asked)?;
        if !opened.metadata().map_err(cannot)?.is_dir() {
            return Err(ToolError::invalid(format!(
                "`path` must name a .canvas file in a folder, and {} is not a folder.",
                folder.display()
            )));
        }
        Ok((opened, name))
    }

    /// The text of the canvas file at `asked`, read only when it is a regular `.canvas` file
    /// inside the folders.
    fn read_canvas(&self, asked: &str) -> Result<String, ToolError> {
        let real = self.canvas_file(asked)?;
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

fn is_canvas(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("canvas")
}

pub(crate) fn exists(asked: &str) -> ToolError {
    ToolError::new(
        "exists",
        format!(
            "{asked} exists already: change it with canvas_edit (read it first with canvas_read), or give another path."
        ),
    )
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
pub(crate) struct ToolError {
    pub(crate) code: &'static str,
    message: String,
    /// For an `io` error, the system's own words.
    cause: String,
}

impl ToolError {
    pub(crate) fn new(code: &'static str, message: String) -> Self {
        Self {
            code,
            message,
            cause: String::new(),
        }
    }

    pub(crate) fn invalid(message: String) -> Self {
        Self::new("invalid", message)
    }

    fn io(asked: &str, error: &io::Error) -> Self {
        Self::io_doing(asked, "read", error)
    }

    /// The system's error, met while doing this (`read`, `written`) to `asked`.
    pub(crate) fn io_doing(asked: &str, doing: &str, error: &io::Error) -> Self {
        let hint = match error.kind() {
            io::ErrorKind::NotFound => {
                " Check the path: it is relative to the folder the server started in, or absolute."
            }
            _ => "",
        };
        Self {
            cause: error.to_string(),
            ..Self::new("io", format!("{asked} cannot be {doing}: {error}.{hint}"))
        }
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
    fn serve(&mut self, input: impl BufRead, output: impl Write) -> io::Result<()> {
        self.serve_lines(input, output, LONGEST_LINE)
    }

    /// [`Server::serve`], a line read up to `longest` bytes.
    fn serve_lines(
        &mut self,
        mut input: impl BufRead,
        mut output: impl Write,
        longest: usize,
    ) -> io::Result<()> {
        let mut line = Vec::new();
        loop {
            let answer = match read_line(&mut input, &mut line, longest)? {
                Line::End => return Ok(()),
                Line::TooLong => Some(failure(
                    Value::Null,
                    RpcError::new(
                        PARSE_ERROR,
                        format!(
                            "Parse error: the message is longer than {longest} bytes, and was not read"
                        ),
                    ),
                )),
                Line::Whole if line.iter().all(u8::is_ascii_whitespace) => continue,
                Line::Whole => self.answer(&line),
            };
            if let Some(answer) = answer {
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

/// What [`read_line`] read.
#[derive(Debug, PartialEq)]
enum Line {
    /// The input ended before a line began.
    End,
    /// A line, whole, its end of line included if it has one.
    Whole,
    /// A line longer than the bound: read to its end, and not kept.
    TooLong,
}

/// Reads the next line of `input` into `line`, at most `longest` bytes of it: a longer line is
/// read on to its end (or the input's) without being kept.
fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>, longest: usize) -> io::Result<Line> {
    line.clear();
    let (mut began, mut too_long) = (false, false);
    loop {
        let available = match input.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(match (began, too_long) {
                (false, _) => Line::End,
                (true, false) => Line::Whole,
                (true, true) => Line::TooLong,
            });
        }
        began = true;
        let (used, ends) = match available.iter().position(|&b| b == b'\n') {
            Some(end) => (end + 1, true),
            None => (available.len(), false),
        };
        if !too_long {
            if line.len() + used > longest {
                too_long = true;
                line.clear();
                line.shrink_to_fit();
            } else {
                line.extend_from_slice(&available[..used]);
            }
        }
        input.consume(used);
        if ends {
            return Ok(if too_long { Line::TooLong } else { Line::Whole });
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
    result.insert(
        "tools".into(),
        json!([
            canvas_read_tool(),
            mcp_edit::canvas_edit_tool(),
            mcp_edit::canvas_create_tool()
        ]),
    );
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
                "path": path_schema("The .canvas file"),
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

/// The schema of a tool's `path`, `what` saying what it names.
pub(crate) fn path_schema(what: &str) -> Value {
    json!({
        "type": "string",
        "description": format!("{what}: relative to the folder the server started in, or absolute. It must be inside the folders the server may use."),
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
        "canvas_edit" => mcp_edit::canvas_edit(folders, arguments),
        "canvas_create" => mcp_edit::canvas_create(folders, arguments),
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

/// Refuses an argument `tool` does not take; `takes` says those it does.
pub(crate) fn only(
    arguments: &Map<String, Value>,
    tool: &str,
    allowed: &[&str],
    takes: &str,
) -> Result<(), ToolError> {
    match arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Some(key) => Err(ToolError::invalid(format!(
            "`{key}` is not an argument of {tool}, which takes {takes}."
        ))),
        None => Ok(()),
    }
}

/// The `path` argument, required.
pub(crate) fn path_argument<'a>(
    folders: &Folders,
    arguments: &'a Map<String, Value>,
) -> Result<&'a str, ToolError> {
    match arguments.get("path") {
        Some(Value::String(path)) if !path.is_empty() => Ok(path.as_str()),
        None | Some(Value::Null | Value::String(_)) => Err(ToolError::invalid(format!(
            "`path` is required: the .canvas file, relative to {} or absolute.",
            folders.start.display()
        ))),
        Some(other) => Err(ToolError::invalid(format!(
            "`path` must be a string, not {other}."
        ))),
    }
}

fn canvas_read(folders: &Folders, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    only(
        arguments,
        "canvas_read",
        &["path", "ids", "text"],
        "`path`, `ids` and `text`",
    )?;
    let path = path_argument(folders, arguments)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A folder checked, then swapped for a link to one outside before the write: refused when
    /// the swap comes before the folder is opened, and nothing written outside when it comes
    /// after.
    #[test]
    fn a_folder_swapped_for_a_link_after_the_check_writes_nothing_outside() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("rature-mcp-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (work, elsewhere) = (root.join("work"), root.join("elsewhere"));
        fs::create_dir_all(work.join("d")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let canvas = r#"{"nodes":[],"edges":[]}"#;
        fs::write(work.join("d/a.canvas"), canvas).unwrap();
        fs::write(elsewhere.join("a.canvas"), canvas).unwrap();
        let folders = Folders::new(&work, &[]).unwrap();
        let swap = || {
            fs::rename(work.join("d"), work.join("d2")).unwrap();
            symlink(&elsewhere, work.join("d")).unwrap();
        };

        let real = folders.canvas_file("d/a.canvas").unwrap();
        swap();
        let error = folders.open_folder(&real, "d/a.canvas").unwrap_err();
        assert_eq!(error.code, "outside", "{error}");
        fs::remove_file(work.join("d")).unwrap();
        fs::rename(work.join("d2"), work.join("d")).unwrap();

        let real = folders.canvas_file("d/a.canvas").unwrap();
        let (opened, name) = folders.open_folder(&real, "d/a.canvas").unwrap();
        swap();
        let done = crate::canvas_board::update_in(&opened, name, &root.join("locks"), |now| {
            Some(now.replace("[]}", "[],\"mine\":1}"))
        })
        .unwrap();
        assert!(matches!(done, fsio::Update::Written { .. }), "{done:?}");
        assert!(
            fs::read_to_string(work.join("d2/a.canvas"))
                .unwrap()
                .contains("mine")
        );
        assert_eq!(
            fs::read_to_string(elsewhere.join("a.canvas")).unwrap(),
            canvas
        );
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_line_past_the_bound_is_skipped_unkept_and_the_next_one_read() {
        // A buffer smaller than a line: the line comes in pieces.
        let text = b"0123456789\nabcdefghijklmnopqrstuvwxyz\nend".to_vec();
        let mut input = io::BufReader::with_capacity(4, io::Cursor::new(text));
        let mut line = Vec::new();
        assert_eq!(read_line(&mut input, &mut line, 11).unwrap(), Line::Whole);
        assert_eq!(line, b"0123456789\n", "at the bound, its end included");
        assert_eq!(read_line(&mut input, &mut line, 11).unwrap(), Line::TooLong);
        assert!(line.is_empty());
        assert_eq!(read_line(&mut input, &mut line, 11).unwrap(), Line::Whole);
        assert_eq!(line, b"end");
        assert_eq!(read_line(&mut input, &mut line, 11).unwrap(), Line::End);
        // A line that never ends, past the bound.
        let mut input = io::Cursor::new(vec![b'x'; 100]);
        assert_eq!(read_line(&mut input, &mut line, 11).unwrap(), Line::TooLong);
        assert_eq!(read_line(&mut input, &mut line, 11).unwrap(), Line::End);
    }

    #[test]
    fn a_message_past_the_bound_gets_a_parse_error_and_the_next_one_its_answer() {
        let mut server = Server {
            folders: Folders {
                start: "/nowhere".into(),
                allowed: Vec::new(),
            },
            legacy: None,
        };
        let ping = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        let input = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{}\"}}\n{ping}\n",
            "x".repeat(200)
        );
        let mut output = Vec::new();
        server
            .serve_lines(io::Cursor::new(input), &mut output, 100)
            .unwrap();
        let answers: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert_eq!(answers[0]["id"], Value::Null);
        assert_eq!(answers[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(answers[1], json!({"jsonrpc": "2.0", "id": 7, "result": {}}));
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
