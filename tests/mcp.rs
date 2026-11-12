//! `rature mcp`, driven through its pipes as a client drives it: in both eras of MCP, each tool
//! error, and the allowed folders. Every line it writes on stdout must be a JSON-RPC message.
//! It runs in folders made for each test, with HOME and XDG folders of their own.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_rature");
const SAMPLE: &str = include_str!("fixtures/jsoncanvas/sample.canvas");

/// A canvas with fields Rature does not know, a node type it does not know, and a text of two
/// lines.
const ODD: &str = r#"{"meta":1,"nodes":[{"id":"a","type":"text","text":"one\ntwo","x":0,"y":0,"width":10,"height":10,"zz":{"k":[1]}},{"id":"b","type":"shape","x":20,"y":0,"width":10,"height":10}],"edges":[{"id":"e","fromNode":"a","toNode":"b","bend":0.5}]}"#;

/// A folder of the test's own, removed after it: `work`, where the server starts, and
/// `elsewhere`, outside it.
struct Place {
    root: PathBuf,
}

impl Place {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "mcp-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["work", "elsewhere", "home", "xdg"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self {
            root: fs::canonicalize(root).unwrap(),
        }
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn elsewhere(&self) -> PathBuf {
        self.root.join("elsewhere")
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// Runs `rature mcp` with these arguments in `work`, writes `input` (one message a line),
    /// closes stdin, and waits for it to end.
    fn run(&self, args: &[&str], input: &[String]) -> (String, String, ExitStatus) {
        let xdg = self.root.join("xdg");
        let mut child = Command::new(BIN)
            .arg("mcp")
            .args(args)
            .current_dir(self.work())
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", xdg.join("config"))
            .env("XDG_DATA_HOME", xdg.join("data"))
            .env("XDG_STATE_HOME", xdg.join("state"))
            .env("XDG_CACHE_HOME", xdg.join("cache"))
            .env("XDG_RUNTIME_DIR", xdg.join("runtime"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the binary starts");
        let mut stdin = child.stdin.take().unwrap();
        let mut text = input.join("\n");
        text.push('\n');
        let writer = std::thread::spawn(move || {
            // The server may have ended already (a bad argument): its pipe is then closed.
            let _ = stdin.write_all(text.as_bytes());
        });
        let mut stdout = child.stdout.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut text = String::new();
            stdout.read_to_string(&mut text).unwrap();
            text
        });
        let mut stderr = child.stderr.take().unwrap();
        let err = std::thread::spawn(move || {
            let mut text = String::new();
            stderr.read_to_string(&mut text).unwrap();
            text
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("rature mcp did not end when its stdin did");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        writer.join().unwrap();
        (out.join().unwrap(), err.join().unwrap(), status)
    }

    /// The messages `rature mcp` answers to `input`, each line of its stdout checked.
    fn session(&self, args: &[&str], input: &[Value]) -> Vec<Value> {
        let lines: Vec<String> = input.iter().map(Value::to_string).collect();
        self.raw_session(args, &lines)
    }

    fn raw_session(&self, args: &[&str], input: &[String]) -> Vec<Value> {
        let (out, err, status) = self.run(args, input);
        assert!(status.success(), "{status}, stderr: {err}");
        assert!(out.is_empty() || out.ends_with('\n'), "{out:?}");
        out.lines().map(json_rpc).collect()
    }

    /// A `canvas_read` call's result, in the modern era and in the legacy one, which must agree.
    fn read(&self, args: &[&str], arguments: Value) -> Value {
        let answers = self.session(
            args,
            &[
                modern(
                    1,
                    "tools/call",
                    json!({"name": "canvas_read", "arguments": arguments}),
                ),
                initialize("2025-11-25"),
                initialized(),
                legacy(
                    2,
                    "tools/call",
                    json!({"name": "canvas_read", "arguments": arguments}),
                ),
            ],
        );
        assert_eq!(answers.len(), 3, "{answers:?}");
        let modern = result(&answers, 1);
        let legacy = result(&answers, 2);
        assert_eq!(modern["resultType"], "complete");
        assert_eq!(
            modern["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "rature"
        );
        let mut bare = modern.clone();
        let bare = bare.as_object_mut().unwrap();
        bare.remove("resultType");
        bare.remove("_meta");
        assert_eq!(&Value::Object(bare.clone()), legacy);
        legacy.clone()
    }

    /// The structured output of a `canvas_read` that worked.
    fn output(&self, args: &[&str], arguments: Value) -> Value {
        let result = self.read(args, arguments);
        assert_eq!(result["isError"], false, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        // The same JSON in text, for the clients that read only that.
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            result["structuredContent"]
        );
        let output = result["structuredContent"].clone();
        let revision = output["revision"].as_str().unwrap();
        assert_eq!(revision.len(), 32, "{revision}");
        assert!(revision.bytes().all(|b| b.is_ascii_hexdigit()));
        output
    }

    /// The text of a `canvas_read` that failed, which ends with `[code]`.
    fn error(&self, args: &[&str], arguments: Value, code: &str) -> String {
        let result = self.read(args, arguments);
        assert_eq!(result["isError"], true, "{result}");
        assert!(result.get("structuredContent").is_none(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        assert!(text.ends_with(&format!(" [{code}]")), "{text}");
        text
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A line of stdout, which must be one JSON-RPC response.
fn json_rpc(line: &str) -> Value {
    let message: Value = serde_json::from_str(line)
        .unwrap_or_else(|error| panic!("not JSON on stdout ({error}): {line:?}"));
    let object = message
        .as_object()
        .expect("a JSON-RPC message is an object");
    assert_eq!(object.get("jsonrpc"), Some(&json!("2.0")), "{line}");
    let id = object.get("id").expect("a response has an id");
    assert!(id.is_string() || id.is_i64() || id.is_null(), "{line}");
    assert!(
        object.contains_key("result") != object.contains_key("error"),
        "a result or an error: {line}"
    );
    if let Some(error) = object.get("error") {
        assert!(error["code"].is_i64(), "{line}");
        assert!(error["message"].is_string(), "{line}");
    }
    message
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
    })
}

fn modern(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = meta();
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn legacy(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialize(version: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        },
    })
}

fn initialized() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

fn answer(answers: &[Value], id: i64) -> &Value {
    answers
        .iter()
        .find(|answer| answer["id"] == id)
        .unwrap_or_else(|| panic!("no answer to {id}: {answers:?}"))
}

fn result(answers: &[Value], id: i64) -> &Value {
    let answer = answer(answers, id);
    answer
        .get("result")
        .unwrap_or_else(|| panic!("an error: {answer}"))
}

fn error_code(answers: &[Value], id: i64) -> i64 {
    let answer = answer(answers, id);
    answer["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not an error: {answer}"))
}

fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect()
}

#[test]
fn the_legacy_era_initializes_pings_and_lists_its_tool() {
    let place = Place::new("legacy");
    let answers = place.session(
        &[],
        &[
            initialize("2025-11-25"),
            initialized(),
            legacy(1, "ping", json!({})),
            legacy(2, "tools/list", json!({})),
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
        ],
    );
    // Nothing answers the notification.
    assert_eq!(answers.len(), 4, "{answers:?}");
    let init = result(&answers, 0);
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert!(init["capabilities"]["tools"].is_object());
    assert_eq!(init["serverInfo"]["name"], "rature");
    assert!(
        init["instructions"]
            .as_str()
            .unwrap()
            .contains("canvas_read")
    );
    assert!(init.get("resultType").is_none());
    assert_eq!(result(&answers, 1), &json!({}));
    assert_eq!(result(&answers, 3), &json!({}));
    let list = result(&answers, 2);
    assert!(list.get("ttlMs").is_none());
    let tools = list["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["canvas_read", "canvas_edit", "canvas_create"]);
    let tool = &tools[0];
    assert_eq!(tool["inputSchema"]["type"], "object");
    assert_eq!(tool["inputSchema"]["required"], json!(["path"]));
    assert_eq!(
        tool["inputSchema"]["properties"]["text"]["enum"],
        json!(["full", "summary"])
    );
    assert_eq!(
        tool["outputSchema"]["required"],
        json!(["revision", "nodes", "edges"])
    );
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);
}

#[test]
fn initialize_agrees_to_a_legacy_version_it_knows_and_else_to_its_latest() {
    let place = Place::new("versions");
    for (asked, agreed) in [
        ("2025-11-25", "2025-11-25"),
        ("2025-06-18", "2025-06-18"),
        // Not a version `initialize` opens.
        ("2026-07-28", "2025-11-25"),
        ("1999-01-01", "2025-11-25"),
    ] {
        let answers = place.session(&[], &[initialize(asked)]);
        assert_eq!(result(&answers, 0)["protocolVersion"], agreed, "{asked}");
    }
}

#[test]
fn ping_answers_before_initialize() {
    let place = Place::new("ping");
    let answers = place.session(&[], &[legacy(1, "ping", json!({}))]);
    assert_eq!(result(&answers, 1), &json!({}));
}

#[test]
fn the_modern_era_discovers_and_lists_without_a_handshake() {
    let place = Place::new("modern");
    let answers = place.session(
        &[],
        &[
            modern(1, "server/discover", json!({})),
            modern(2, "tools/list", json!({})),
            // Both eras in one process: a legacy client, then the modern one again.
            initialize("2025-11-25"),
            initialized(),
            legacy(3, "tools/list", json!({})),
            modern(4, "tools/list", json!({})),
        ],
    );
    assert_eq!(answers.len(), 5, "{answers:?}");
    let discover = result(&answers, 1);
    assert_eq!(discover["resultType"], "complete");
    assert_eq!(
        discover["supportedVersions"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18"])
    );
    assert!(discover["capabilities"]["tools"].is_object());
    assert!(
        discover["instructions"]
            .as_str()
            .unwrap()
            .contains("canvas_read")
    );
    assert!(discover["ttlMs"].as_u64().is_some());
    assert_eq!(discover["cacheScope"], "public");
    let info = &discover["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(info["name"], "rature");
    assert!(info["version"].is_string());
    for id in [2, 4] {
        let list = result(&answers, id);
        assert_eq!(list["resultType"], "complete");
        assert!(list["ttlMs"].as_u64().is_some());
        assert_eq!(list["cacheScope"], "public");
        assert_eq!(
            list["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "rature"
        );
        // The same tools in both eras.
        assert_eq!(list["tools"], result(&answers, 3)["tools"]);
    }
}

#[test]
fn a_modern_request_is_checked_against_its_meta() {
    let place = Place::new("meta");
    let mut no_capabilities = modern(2, "tools/list", json!({}));
    no_capabilities["params"]["_meta"]
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientCapabilities");
    let mut old = modern(3, "server/discover", json!({}));
    old["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let mut not_a_string = modern(6, "tools/list", json!({}));
    not_a_string["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!(20260728);
    let answers = place.session(
        &[],
        &[
            // No `_meta`, and no `initialize` before.
            legacy(1, "tools/list", json!({})),
            no_capabilities,
            old,
            // Gone from the modern era.
            modern(4, "ping", json!({})),
            modern(5, "resources/list", json!({})),
            not_a_string,
        ],
    );
    assert_eq!(answers.len(), 6, "{answers:?}");
    assert_eq!(error_code(&answers, 1), -32602);
    assert_eq!(error_code(&answers, 2), -32602);
    assert_eq!(error_code(&answers, 3), -32022);
    let data = &answer(&answers, 3)["error"]["data"];
    assert_eq!(data["requested"], "1900-01-01");
    assert!(
        data["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    assert_eq!(error_code(&answers, 4), -32601);
    assert_eq!(error_code(&answers, 5), -32601);
    assert_eq!(error_code(&answers, 6), -32602);
}

#[test]
fn malformed_messages_get_json_rpc_errors_and_notifications_nothing() {
    let place = Place::new("malformed");
    let lines: Vec<String> = [
        "not json".to_owned(),
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.to_owned(),
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"notifications/anything"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}}"#
            .to_owned(),
        // A response: the server asked nothing.
        r#"{"jsonrpc":"2.0","id":7,"result":{}}"#.to_owned(),
        String::new(),
        r#"{"jsonrpc":"1.0","id":10,"method":"ping"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","id":11,"method":"ping","params":[]}"#.to_owned(),
        modern(
            12,
            "tools/call",
            json!({"name": "canvas_write", "arguments": {}}),
        )
        .to_string(),
        modern(13, "tools/call", json!({"arguments": {}})).to_string(),
        modern(
            14,
            "tools/call",
            json!({"name": "canvas_read", "arguments": "x"}),
        )
        .to_string(),
        modern(15, "tools/list", json!({"cursor": "next"})).to_string(),
        // Still serving after all that.
        modern(16, "tools/list", json!({})).to_string(),
    ]
    .into();
    let answers = place.raw_session(&[], &lines);
    assert_eq!(answers.len(), 10, "{answers:?}");
    let nulls: Vec<i64> = answers
        .iter()
        .filter(|answer| answer["id"].is_null())
        .map(|answer| answer["error"]["code"].as_i64().unwrap())
        .collect();
    assert_eq!(nulls, [-32700, -32600, -32600]);
    assert_eq!(error_code(&answers, 10), -32600);
    assert_eq!(error_code(&answers, 11), -32602);
    assert_eq!(error_code(&answers, 12), -32602);
    assert!(
        answer(&answers, 12)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("canvas_write")
    );
    assert_eq!(error_code(&answers, 13), -32602);
    assert_eq!(error_code(&answers, 14), -32602);
    assert_eq!(error_code(&answers, 15), -32602);
    assert!(result(&answers, 16)["tools"].is_array());
}

#[test]
fn canvas_read_gives_every_node_and_edge_whole_in_the_files_order() {
    let place = Place::new("full");
    let sample = place.write("work/sample.canvas", SAMPLE);
    let output = place.output(&[], json!({"path": "sample.canvas"}));
    assert_eq!(
        ids(&output["nodes"]),
        [
            "754a8ef995f366bc",
            "8132d4d894c80022",
            "7efdbbe0c4742315",
            "59e896bc8da20699",
            "0ba565e7f30e0652",
        ]
    );
    assert_eq!(ids(&output["edges"]), ["6fa11ab87f90b8af"]);
    let nodes = output["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["type"], "group");
    assert_eq!(nodes[1]["color"], "6");
    assert_eq!(output["edges"][0]["fromSide"], "right");
    assert_eq!(nodes[0]["label"], "JSON Canvas");
    assert!(
        nodes[3]["text"]
            .as_str()
            .unwrap()
            .starts_with("Learn more:\n\n- [Apps]")
    );
    // An absolute path reads the same file.
    let absolute = place.output(
        &[],
        json!({"path": sample.to_str().unwrap(), "text": "full"}),
    );
    assert_eq!(absolute, output);

    place.write("work/odd.canvas", ODD);
    let odd = place.output(&[], json!({"path": "odd.canvas"}));
    assert_eq!(ids(&odd["nodes"]), ["a", "b"]);
    assert_eq!(odd["nodes"][0]["zz"], json!({"k": [1]}));
    assert_eq!(odd["nodes"][0]["text"], "one\ntwo");
    assert_eq!(odd["nodes"][1]["type"], "shape");
    assert_eq!(
        odd["edges"],
        json!([{"id": "e", "fromNode": "a", "toNode": "b", "bend": 0.5}])
    );
    assert_ne!(odd["revision"], output["revision"]);
}

#[test]
fn the_revision_follows_the_files_text() {
    let place = Place::new("revision");
    place.write("work/a.canvas", ODD);
    let first = place.output(&[], json!({"path": "a.canvas"}))["revision"].clone();
    let again = place.output(&[], json!({"path": "a.canvas"}))["revision"].clone();
    assert_eq!(first, again);
    place.write("work/a.canvas", &format!("{ODD}\n"));
    let changed = place.output(&[], json!({"path": "a.canvas"}))["revision"].clone();
    assert_ne!(first, changed);
}

#[test]
fn a_summary_gives_the_first_line_of_each_text() {
    let place = Place::new("summary");
    place.write("work/odd.canvas", ODD);
    let summary = place.output(&[], json!({"path": "odd.canvas", "text": "summary"}));
    let full = place.output(&[], json!({"path": "odd.canvas"}));
    assert_eq!(summary["nodes"][0]["text"], "one");
    assert_eq!(summary["nodes"][0]["zz"], json!({"k": [1]}));
    assert_eq!(summary["nodes"][1], full["nodes"][1]);
    assert_eq!(summary["edges"], full["edges"]);
    assert_eq!(summary["revision"], full["revision"]);
}

#[test]
fn ids_keep_the_nodes_and_edges_named_and_an_unknown_one_is_not_found() {
    let place = Place::new("ids");
    place.write("work/odd.canvas", ODD);
    let some = place.output(&[], json!({"path": "odd.canvas", "ids": ["e", "b"]}));
    assert_eq!(ids(&some["nodes"]), ["b"]);
    assert_eq!(ids(&some["edges"]), ["e"]);
    let none = place.output(&[], json!({"path": "odd.canvas", "ids": []}));
    assert_eq!(none["nodes"], json!([]));
    assert_eq!(none["edges"], json!([]));
    let text = place.error(
        &[],
        json!({"path": "odd.canvas", "ids": ["a", "zzz", "yyy"]}),
        "not_found",
    );
    assert!(text.contains("yyy, zzz"), "{text}");
    assert!(text.contains("read the canvas again"), "{text}");
}

#[test]
fn invalid_arguments_say_what_is_allowed() {
    let place = Place::new("invalid");
    place.write("work/odd.canvas", ODD);
    place.write("work/notes.md", "# Notes\n");
    fs::create_dir_all(place.work().join("folder.canvas")).unwrap();
    for (arguments, says) in [
        (
            json!({"path": "odd.canvas", "text": "brief"}),
            "\"full\" or \"summary\"",
        ),
        (json!({}), "`path` is required"),
        (json!({"path": ""}), "`path` is required"),
        (json!({"path": 3}), "`path` must be a string"),
        (
            json!({"path": "odd.canvas", "ids": "a"}),
            "`ids` must be a list of strings",
        ),
        (
            json!({"path": "odd.canvas", "ids": [1]}),
            "`ids` must be a list of strings",
        ),
        (
            json!({"path": "odd.canvas", "full": true}),
            "`full` is not an argument",
        ),
        (json!({"path": "notes.md"}), ".canvas file"),
        (json!({"path": "folder.canvas"}), "a folder"),
    ] {
        let text = place.error(&[], arguments.clone(), "invalid");
        assert!(text.contains(says), "{arguments}: {text}");
    }
}

#[test]
fn a_named_pipe_is_refused_without_waiting_for_a_writer() {
    let place = Place::new("fifo");
    let fifo = place.work().join("pipe.canvas");
    let made = Command::new("mkfifo").arg(&fifo).status();
    if !made.is_ok_and(|status| status.success()) {
        eprintln!("mkfifo is missing: not tested");
        return;
    }
    let text = place.error(&[], json!({"path": "pipe.canvas"}), "invalid");
    assert!(text.contains("not a regular file"), "{text}");
}

#[test]
fn a_file_that_is_not_a_canvas_says_where_it_breaks() {
    let place = Place::new("broken");
    place.write("work/broken.canvas", "{\n\t\"nodes\":[\n");
    let text = place.error(&[], json!({"path": "broken.canvas"}), "not_a_canvas");
    assert!(text.contains("line"), "{text}");
    fs::write(
        place.work().join("latin1.canvas"),
        b"{\"nodes\":[],\"x\":\"\xe9\"}",
    )
    .unwrap();
    let text = place.error(&[], json!({"path": "latin1.canvas"}), "not_a_canvas");
    assert!(text.contains("UTF-8"), "{text}");
}

#[test]
fn a_canvas_with_flaws_is_read_with_its_problems() {
    let place = Place::new("flaws");
    place.write(
        "work/twice.canvas",
        r#"{"nodes":[{"id":"a","type":"text","text":"x"},{"id":"a","type":"text","text":"y"}],"edges":[{"id":"e","fromNode":"a","toNode":"gone"}]}"#,
    );
    let output = place.output(&[], json!({"path": "twice.canvas"}));
    assert_eq!(ids(&output["nodes"]), ["a", "a"]);
    assert_eq!(ids(&output["edges"]), ["e"]);
    let problems: Vec<&str> = output["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].starts_with("The node id a"), "{problems:?}");
    assert!(problems[1].contains("gone"), "{problems:?}");
    assert!(problems.iter().all(|p| p.ends_with('.')), "{problems:?}");
    // A file that holds JSON, but no canvas: nothing in it, and why.
    place.write("work/list.canvas", "[1]");
    let output = place.output(&[], json!({"path": "list.canvas"}));
    assert_eq!(output["nodes"], json!([]));
    assert_eq!(output["problems"].as_array().unwrap().len(), 1);
    // A sound canvas has none.
    place.write("work/odd.canvas", ODD);
    let output = place.output(&[], json!({"path": "odd.canvas"}));
    assert!(output.get("problems").is_none(), "{output}");
}

#[test]
fn a_large_canvas_comes_as_a_summary_unless_full_is_asked() {
    let place = Place::new("large");
    let body = "word ".repeat(100);
    let nodes: Vec<Value> = (0..150)
        .map(|n| {
            json!({"id": format!("n{n}"), "type": "text", "text": format!("Card {n}\n{body}"),
                "x": n * 10, "y": 0, "width": 100, "height": 100})
        })
        .collect();
    let canvas = json!({"nodes": nodes, "edges": []}).to_string();
    assert!(canvas.len() > 60_000);
    place.write("work/large.canvas", &canvas);

    let unasked = place.output(&[], json!({"path": "large.canvas"}));
    assert_eq!(unasked["truncated"], true);
    let note = unasked["note"].as_str().unwrap();
    assert!(
        note.contains("`ids`") && note.contains("\"full\""),
        "{note}"
    );
    assert_eq!(unasked["nodes"].as_array().unwrap().len(), 150);
    assert_eq!(unasked["nodes"][7]["text"], "Card 7");

    let full = place.output(&[], json!({"path": "large.canvas", "text": "full"}));
    assert!(full.get("truncated").is_none());
    assert_eq!(full["nodes"][7]["text"], format!("Card 7\n{body}"));
    assert_eq!(full["revision"], unasked["revision"]);

    let summary = place.output(&[], json!({"path": "large.canvas", "text": "summary"}));
    assert!(summary.get("truncated").is_none());
    assert_eq!(summary["nodes"], unasked["nodes"]);

    // A few cards of it fit whole.
    let some = place.output(&[], json!({"path": "large.canvas", "ids": ["n7"]}));
    assert!(some.get("truncated").is_none());
    assert_eq!(some["nodes"][0]["text"], format!("Card 7\n{body}"));
}

#[test]
fn a_missing_file_is_the_systems_error() {
    let place = Place::new("io");
    let text = place.error(&[], json!({"path": "missing.canvas"}), "io");
    assert!(text.contains("No such file"), "{text}");
}

#[test]
fn a_path_outside_the_allowed_folders_is_refused_through_a_link_too() {
    let place = Place::new("outside");
    let out = place.write("elsewhere/out.canvas", ODD);
    place.write("work2/near.canvas", ODD);
    place.write("work/in.canvas", ODD);
    std::os::unix::fs::symlink(&out, place.work().join("link.canvas")).unwrap();
    std::os::unix::fs::symlink(place.elsewhere(), place.work().join("linked")).unwrap();
    std::os::unix::fs::symlink(
        place.work().join("in.canvas"),
        place.work().join("inner.canvas"),
    )
    .unwrap();
    let work = place.work();
    for path in [
        out.to_str().unwrap(),
        "../elsewhere/out.canvas",
        "link.canvas",
        "linked/out.canvas",
        // Its name begins as `work`'s: not in it for that.
        "../work2/near.canvas",
        // Not there, and outside: said to be outside rather than missing.
        "../elsewhere/none.canvas",
        "/",
    ] {
        let text = place.error(&[], json!({"path": path}), "outside");
        assert!(text.contains(work.to_str().unwrap()), "{path}: {text}");
        assert!(text.contains("--allow-dir"), "{path}: {text}");
    }
    // A link that stays inside is followed.
    let inner = place.output(&[], json!({"path": "inner.canvas"}));
    assert_eq!(ids(&inner["nodes"]), ["a", "b"]);

    // Allowed, the other folder is read, by any of these paths.
    let elsewhere = place.elsewhere();
    let allow = ["--allow-dir", elsewhere.to_str().unwrap()];
    for path in [out.to_str().unwrap(), "link.canvas", "linked/out.canvas"] {
        let output = place.output(&allow, json!({"path": path}));
        assert_eq!(ids(&output["nodes"]), ["a", "b"], "{path}");
    }
    let text = place.error(&allow, json!({"path": "../work2/near.canvas"}), "outside");
    assert!(text.contains(elsewhere.to_str().unwrap()), "{text}");
}

#[test]
fn the_arguments_are_checked_before_serving() {
    let place = Place::new("args");
    let (out, err, status) = place.run(&["--allow-dir", "/no/such/folder"], &[]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(out, "");
    assert!(err.contains("--allow-dir /no/such/folder"), "{err}");
    let file = place.write("work/odd.canvas", ODD);
    let (out, err, status) = place.run(&["--allow-dir", file.to_str().unwrap()], &[]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(out, "");
    assert!(err.contains("not a folder"), "{err}");
    let (out, err, status) = place.run(&["--bogus"], &[]);
    assert_eq!(status.code(), Some(2));
    assert_eq!(out, "");
    assert!(err.contains("usage: rature mcp"), "{err}");
    let (out, _, status) = place.run(&["--help"], &[]);
    assert!(status.success());
    assert!(out.starts_with("usage: rature mcp"), "{out}");
}

#[test]
fn a_message_past_16_mib_is_not_read_and_the_next_one_is_served() {
    let place = Place::new("long");
    // A request the server would answer, were it read: a ping with a long, unknown field.
    let long = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "x".repeat(16 * 1024 * 1024)
    );
    let lines = [
        long,
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#.to_owned(),
    ];
    let answers = place.raw_session(&[], &lines);
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert!(answers[0]["id"].is_null(), "{}", answers[0]);
    assert_eq!(answers[0]["error"]["code"], -32700);
    assert_eq!(result(&answers, 2), &json!({}));
}

#[test]
fn the_server_ends_when_stdin_does() {
    let place = Place::new("eof");
    let (out, _, status) = place.run(&[], &[]);
    assert!(status.success());
    assert_eq!(out, "");
}

// canvas_edit and canvas_create.

impl Place {
    /// A tool's result, called in the modern era.
    fn call(&self, args: &[&str], tool: &str, arguments: Value) -> Value {
        let answers = self.session(
            args,
            &[modern(
                1,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )],
        );
        assert_eq!(answers.len(), 1, "{answers:?}");
        result(&answers, 1).clone()
    }

    /// The structured output of a call that worked.
    fn done(&self, tool: &str, arguments: Value) -> Value {
        let result = self.call(&[], tool, arguments);
        assert_eq!(result["isError"], false, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            result["structuredContent"]
        );
        let output = result["structuredContent"].clone();
        for key in ["revision", "created", "changed", "deleted"] {
            assert!(output.get(key).is_some(), "{key}: {output}");
        }
        output
    }

    fn edit(&self, path: &str, ops: Value) -> Value {
        self.done("canvas_edit", json!({"path": path, "ops": ops}))
    }

    /// The text of a call that failed, which ends with `[code]`.
    fn refused_with(&self, args: &[&str], tool: &str, arguments: Value, code: &str) -> String {
        let result = self.call(args, tool, arguments);
        assert_eq!(result["isError"], true, "{result}");
        assert!(result.get("structuredContent").is_none(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        assert!(text.ends_with(&format!(" [{code}]")), "{text}");
        text
    }

    fn refused(&self, tool: &str, arguments: Value, code: &str) -> String {
        self.refused_with(&[], tool, arguments, code)
    }

    /// `canvas_edit` refused: the file is left byte for byte.
    fn edit_refused(&self, path: &str, ops: Value, code: &str) -> String {
        let file = self.work().join(path);
        let before = fs::read(&file).unwrap();
        let text = self.refused("canvas_edit", json!({"path": path, "ops": ops}), code);
        assert_eq!(fs::read(&file).unwrap(), before, "written: {text}");
        text
    }

    /// The canvas at `relative` in `work`, as JSON.
    fn canvas(&self, relative: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(self.work().join(relative)).unwrap()).unwrap()
    }
}

/// The node or edge with this id.
fn by_id<'a>(list: &'a Value, id: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("no {id} in {list}"))
}

fn created<'a>(output: &'a Value, name: &str) -> &'a str {
    output["created"][name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} not created: {output}"))
}

/// The cards of a canvas: none overlaps another.
fn assert_apart(canvas: &Value) {
    let rects: Vec<(f64, f64, f64, f64, &str)> = canvas["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let f = |k: &str| n[k].as_f64().unwrap();
            (
                f("x"),
                f("y"),
                f("width"),
                f("height"),
                n["id"].as_str().unwrap(),
            )
        })
        .collect();
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let overlap = a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3;
            assert!(!overlap, "{} overlaps {}: {canvas}", a.4, b.4);
        }
    }
}

const THREE: &str = r#"{"nodes":[{"id":"a","type":"text","text":"Alpha beta","x":0,"y":0,"width":250,"height":60,"zz":1},{"id":"f","type":"file","file":"notes.md","x":300,"y":0,"width":400,"height":400},{"id":"g","type":"group","label":"G","x":-50,"y":-50,"width":800,"height":500}],"edges":[{"id":"af","fromNode":"a","toNode":"f"},{"id":"ag","fromNode":"a","toNode":"g","label":"in"}],"meta":{"by":"test"}}"#;

#[test]
fn the_writing_tools_are_listed_with_their_hints() {
    let place = Place::new("write-list");
    let answers = place.session(&[], &[modern(1, "tools/list", json!({}))]);
    let tools = result(&answers, 1)["tools"].as_array().unwrap().clone();
    let edit = &tools[1];
    assert_eq!(edit["name"], "canvas_edit");
    assert_eq!(edit["inputSchema"]["required"], json!(["path", "ops"]));
    assert_eq!(
        edit["annotations"],
        json!({"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false})
    );
    let create = &tools[2];
    assert_eq!(create["name"], "canvas_create");
    assert_eq!(create["annotations"]["destructiveHint"], false);
    assert_eq!(create["annotations"]["readOnlyHint"], false);
    for tool in [edit, create] {
        assert_eq!(
            tool["outputSchema"]["required"],
            json!(["revision", "created", "changed", "deleted"])
        );
    }
    let answers = place.session(&[], &[modern(2, "server/discover", json!({}))]);
    let instructions = result(&answers, 2)["instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    for says in [
        "canvas_edit",
        "replace",
        "Let Rature place",
        "[stale]",
        "[busy]",
        "[exists]",
    ] {
        assert!(instructions.contains(says), "{says}");
    }
}

#[test]
fn add_node_adds_each_type_of_card_and_add_edge_joins_them_by_ref() {
    let place = Place::new("add");
    place.write("work/a.canvas", THREE);
    let output = place.edit(
        "a.canvas",
        json!([
            {"op": "add_node", "ref": "t", "type": "text", "text": "# New\n\nA card.", "color": "4"},
            {"op": "add_node", "ref": "file", "type": "file", "file": "notes.md", "subpath": "#Part", "near": "t"},
            {"op": "add_node", "ref": "l", "type": "link", "url": "https://example.org", "x": 2000.4, "y": -10, "width": 300, "height": 200},
            {"op": "add_node", "type": "group", "label": "Later"},
            {"op": "add_edge", "ref": "e", "from": "t", "to": "a", "fromSide": "left", "toSide": "right", "toEnd": "none", "label": "see", "color": "#00FF80"},
            {"op": "add_edge", "from": "file", "to": "l"}
        ]),
    );
    let t = created(&output, "t");
    let file = created(&output, "file");
    let l = created(&output, "l");
    let group = created(&output, "ops[3]");
    let e = created(&output, "e");
    let fl = created(&output, "ops[5]");
    for id in [t, file, l, group, e, fl] {
        assert_eq!(id.len(), 16, "{id}");
    }
    assert_eq!(output["changed"], json!([]));
    assert_eq!(output["deleted"], json!([]));

    let canvas = place.canvas("a.canvas");
    let card = by_id(&canvas["nodes"], t);
    assert_eq!(card["type"], "text");
    assert_eq!(card["text"], "# New\n\nA card.");
    assert_eq!(card["color"], "4");
    // Right of all the cards: the group reaches 750.
    assert!(card["x"].as_f64().unwrap() >= 750., "{card}");
    let keys: Vec<&str> = card
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["id", "type", "text", "x", "y", "width", "height", "color"]
    );
    let file_card = by_id(&canvas["nodes"], file);
    assert_eq!(file_card["file"], "notes.md");
    assert_eq!(file_card["subpath"], "#Part");
    // Right of its neighbour.
    assert!(
        file_card["x"].as_f64().unwrap()
            > card["x"].as_f64().unwrap() + card["width"].as_f64().unwrap()
    );
    let link = by_id(&canvas["nodes"], l);
    assert_eq!(link["url"], "https://example.org");
    assert_eq!(
        (link["x"].clone(), link["y"].clone()),
        (json!(2000), json!(-10))
    );
    assert_eq!(
        (link["width"].clone(), link["height"].clone()),
        (json!(300), json!(200))
    );
    assert_eq!(by_id(&canvas["nodes"], group)["label"], "Later");
    let edge = by_id(&canvas["edges"], e);
    assert_eq!(
        edge,
        &json!({"id": e, "fromNode": t, "fromSide": "left", "toNode": "a", "toSide": "right", "toEnd": "none", "color": "#00FF80", "label": "see"})
    );
    assert_eq!(
        by_id(&canvas["edges"], fl),
        &json!({"id": fl, "fromNode": file, "toNode": l})
    );
    // New cards on top, in order; what Rature does not know kept.
    let ids: Vec<&str> = ids(&canvas["nodes"]);
    assert_eq!(ids, ["a", "f", "g", t, file, l, group]);
    assert_eq!(canvas["meta"], json!({"by": "test"}));
    assert_eq!(by_id(&canvas["nodes"], "a")["zz"], 1);
    // The revision given is the file's, as canvas_read gives it.
    let read = place.output(&[], json!({"path": "a.canvas"}));
    assert_eq!(read["revision"], output["revision"]);
    // Written as Rature writes a canvas: one card a line.
    let text = fs::read_to_string(place.work().join("a.canvas")).unwrap();
    assert!(
        text.starts_with("{\n\t\"nodes\":[\n\t\t{\"id\":\"a\""),
        "{text}"
    );
}

#[test]
fn canvas_edit_is_served_in_the_legacy_era_too() {
    let place = Place::new("edit-legacy");
    place.write("work/a.canvas", THREE);
    let answers = place.session(
        &[],
        &[
            initialize("2025-11-25"),
            initialized(),
            legacy(
                1,
                "tools/call",
                json!({"name": "canvas_edit", "arguments": {"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "x": 5}]}}),
            ),
        ],
    );
    let result = result(&answers, 1);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["changed"], json!(["a"]));
    assert_eq!(by_id(&place.canvas("a.canvas")["nodes"], "a")["x"], 5);
}

#[test]
fn update_node_sets_only_the_fields_it_names() {
    let place = Place::new("update");
    place.write("work/a.canvas", THREE);
    let output = place.edit(
        "a.canvas",
        json!([
            {"op": "update_node", "id": "a", "x": 10.6, "color": "#ABCDEF"},
            {"op": "update_node", "id": "f", "width": 420, "height": 380, "file": "other.md", "subpath": "#Top"},
            {"op": "update_node", "id": "g", "label": null, "y": -60},
            // Nothing changes: not said to be changed.
            {"op": "update_node", "id": "a", "text": "Alpha beta"}
        ]),
    );
    assert_eq!(output["changed"], json!(["a", "f", "g"]));
    let canvas = place.canvas("a.canvas");
    let a = by_id(&canvas["nodes"], "a");
    assert_eq!((a["x"].clone(), a["y"].clone()), (json!(11), json!(0)));
    assert_eq!(a["color"], "#ABCDEF");
    assert_eq!(a["text"], "Alpha beta");
    assert_eq!(a["zz"], 1);
    let f = by_id(&canvas["nodes"], "f");
    assert_eq!(
        (f["width"].clone(), f["height"].clone()),
        (json!(420), json!(380))
    );
    assert_eq!(
        (f["file"].clone(), f["subpath"].clone()),
        (json!("other.md"), json!("#Top"))
    );
    let g = by_id(&canvas["nodes"], "g");
    assert!(g.get("label").is_none(), "{g}");
    assert_eq!(g["y"], -60);
    // The colour removed.
    let output = place.edit(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "color": null}]),
    );
    assert_eq!(output["changed"], json!(["a"]));
    assert!(
        by_id(&place.canvas("a.canvas")["nodes"], "a")
            .get("color")
            .is_none()
    );

    // A link card's address, a text card's text.
    place.write("work/b.canvas", r#"{"nodes":[{"id":"l","type":"link","url":"https://a.example","x":0,"y":0,"width":400,"height":300},{"id":"t","type":"text","text":"x","x":500,"y":0,"width":250,"height":60}],"edges":[]}"#);
    place.edit(
        "b.canvas",
        json!([
            {"op": "update_node", "id": "l", "url": "https://b.example"},
            {"op": "update_node", "id": "t", "text": "y"}
        ]),
    );
    let canvas = place.canvas("b.canvas");
    assert_eq!(by_id(&canvas["nodes"], "l")["url"], "https://b.example");
    assert_eq!(by_id(&canvas["nodes"], "t")["text"], "y");

    // A field of another type of card.
    for (op, says) in [
        (
            json!({"op": "update_node", "id": "a", "url": "https://x.example"}),
            "`url` is a field of a link card, and a is a text card",
        ),
        (
            json!({"op": "update_node", "id": "f", "text": "x"}),
            "`text` is a field of a text card",
        ),
        (
            json!({"op": "update_node", "id": "a", "label": "x"}),
            "group card",
        ),
        (
            json!({"op": "update_node", "id": "g", "replace": {"old": "G", "new": "H"}}),
            "`replace` changes a text card's text",
        ),
    ] {
        let text = place.edit_refused("a.canvas", json!([op]), "invalid");
        assert!(text.contains(says), "{text}");
        assert!(text.starts_with("ops[0] (update_node): "), "{text}");
    }
}

#[test]
fn replace_changes_words_only_when_they_are_there_once() {
    let place = Place::new("replace");
    place.write("work/a.canvas", THREE);
    let output = place.edit(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "replace": {"old": "beta", "new": "gamma"}}]),
    );
    assert_eq!(output["changed"], json!(["a"]));
    assert_eq!(
        by_id(&place.canvas("a.canvas")["nodes"], "a")["text"],
        "Alpha gamma"
    );
    // Not there any more: the user changed it.
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "replace": {"old": "beta", "new": "delta"}}]),
        "not_found",
    );
    assert!(text.contains("is not in the text of a"), "{text}");
    // There twice, or more.
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "replace": {"old": "a", "new": "o"}}]),
        "invalid",
    );
    assert!(text.contains("more than once"), "{text}");
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "text": "x", "replace": {"old": "Alpha", "new": "o"}}]),
        "invalid",
    );
    assert!(text.contains("not both"), "{text}");
    // `new` empty deletes the words.
    place.edit(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "replace": {"old": " gamma", "new": ""}}]),
    );
    assert_eq!(
        by_id(&place.canvas("a.canvas")["nodes"], "a")["text"],
        "Alpha"
    );
}

#[test]
fn delete_node_takes_its_edges_and_delete_edge_one_edge() {
    let place = Place::new("delete");
    place.write("work/a.canvas", THREE);
    let output = place.edit(
        "a.canvas",
        json!([
            {"op": "update_node", "id": "g", "x": 0},
            {"op": "delete_node", "id": "g"}
        ]),
    );
    assert_eq!(output["deleted"], json!(["g", "ag"]));
    assert_eq!(output["changed"], json!([]), "changed, then deleted");
    let canvas = place.canvas("a.canvas");
    assert_eq!(ids(&canvas["nodes"]), ["a", "f"]);
    assert_eq!(ids(&canvas["edges"]), ["af"]);
    let output = place.edit("a.canvas", json!([{"op": "delete_edge", "id": "af"}]));
    assert_eq!(output["deleted"], json!(["af"]));
    let canvas = place.canvas("a.canvas");
    assert_eq!(ids(&canvas["nodes"]), ["a", "f"]);
    assert_eq!(canvas["edges"], json!([]));
    // Deleted: not found after.
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "delete_node", "id": "g"}]),
        "not_found",
    );
    assert!(text.contains("no node has the id g"), "{text}");
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "delete_edge", "id": "af"}]),
        "not_found",
    );
    assert!(text.contains("no edge has the id af"), "{text}");
    // A node is not an edge, nor the other way.
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "delete_edge", "id": "a"}]),
        "invalid",
    );
    assert!(text.contains("a node, and takes an edge"), "{text}");
}

#[test]
fn update_edge_sets_its_fields_and_never_an_end_to_no_node() {
    let place = Place::new("update-edge");
    place.write("work/a.canvas", THREE);
    let output = place.edit(
        "a.canvas",
        json!([
            {"op": "add_node", "ref": "n", "type": "text", "text": "N"},
            {"op": "update_edge", "id": "af", "to": "n", "fromEnd": "arrow", "color": "2"},
            {"op": "update_edge", "id": "ag", "label": null, "from": "f"}
        ]),
    );
    let n = created(&output, "n");
    assert_eq!(output["changed"], json!(["af", "ag"]));
    let canvas = place.canvas("a.canvas");
    assert_eq!(
        by_id(&canvas["edges"], "af"),
        &json!({"id": "af", "fromNode": "a", "toNode": n, "fromEnd": "arrow", "color": "2"})
    );
    assert_eq!(
        by_id(&canvas["edges"], "ag"),
        &json!({"id": "ag", "fromNode": "f", "toNode": "g"})
    );
    // An end to no node, or to an edge: refused, nothing written.
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_edge", "id": "af", "to": "zzz"}]),
        "not_found",
    );
    assert!(text.contains("`to`: no node has the id zzz"), "{text}");
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_edge", "id": "af", "from": "ag"}]),
        "invalid",
    );
    assert!(text.contains("an edge, and takes a node"), "{text}");
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_edge", "id": "af", "from": null}]),
        "invalid",
    );
    assert!(text.contains("cannot be removed"), "{text}");
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "add_edge", "from": "a", "to": "nope"}]),
        "not_found",
    );
    assert!(text.contains("nope"), "{text}");
    // A node deleted earlier in the call is no end.
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "delete_node", "id": "f"}, {"op": "add_edge", "from": "a", "to": "f"}]),
        "not_found",
    );
    assert!(text.starts_with("ops[1] (add_edge): "), "{text}");
}

#[test]
fn to_front_and_to_back_change_the_drawing_order() {
    let place = Place::new("order");
    place.write("work/a.canvas", THREE);
    let output = place.edit(
        "a.canvas",
        json!([
            {"op": "to_back", "id": "g"},
            {"op": "to_front", "id": "a"},
            // There already: nothing to say.
            {"op": "to_front", "id": "a"}
        ]),
    );
    assert_eq!(output["changed"], json!(["g", "a"]));
    assert_eq!(ids(&place.canvas("a.canvas")["nodes"]), ["g", "f", "a"]);
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "to_front", "id": "af"}]),
        "invalid",
    );
    assert!(text.contains("an edge, and takes a node"), "{text}");
}

#[test]
fn the_operations_go_all_or_none_and_the_error_names_the_one_that_failed() {
    let place = Place::new("atomic");
    place.write("work/a.canvas", THREE);
    let text = place.edit_refused(
        "a.canvas",
        json!([
            {"op": "add_node", "type": "text", "text": "kept?"},
            {"op": "update_node", "id": "a", "x": 99},
            {"op": "update_node", "id": "missing", "x": 1}
        ]),
        "not_found",
    );
    assert!(text.starts_with("ops[2] (update_node): "), "{text}");
    assert!(text.contains("Nothing was applied."), "{text}");
    assert!(text.contains("canvas_read"), "{text}");
}

#[test]
fn invalid_operations_say_what_is_allowed_before_the_file_is_read() {
    let place = Place::new("edit-invalid");
    place.write("work/a.canvas", THREE);
    place.write("work/notes.md", "# Notes\n");
    for (arguments, says) in [
        (json!({"path": "a.canvas"}), "`ops` is required"),
        (json!({"path": "a.canvas", "ops": []}), "`ops` is empty"),
        (
            json!({"path": "a.canvas", "ops": {}}),
            "`ops` must be a list",
        ),
        (
            json!({"path": "a.canvas", "ops": [], "force": true}),
            "`force` is not an argument of canvas_edit",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "x"}], "if_revision": 3}),
            "`if_revision` must be a string",
        ),
        (
            json!({"path": "notes.md", "ops": [{"op": "to_back", "id": "a"}]}),
            ".canvas file",
        ),
        (
            json!({"path": "a.canvas", "ops": [1]}),
            "ops[0]: an operation must be an object",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "move"}]}),
            "`op` must be one of add_node",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"id": "a"}]}),
            "not nothing",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "text": "x"}]}),
            "`type` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "shape"}]}),
            "`type` must be one of text, file, link, group",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text"}]}),
            "`text` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "file"}]}),
            "`file` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "link"}]}),
            "`url` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "url": "u"}]}),
            "`url` is a field of a link card",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "file", "file": "a.md", "subpath": "Part"}]}),
            "`subpath` must begin with #",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "x": 1}]}),
            "both `x` and `y`",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "x": 1, "y": 1, "near": "a"}]}),
            "`near` places a card",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "width": 0}]}),
            "`width` must be 1 or more",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "x": "1", "y": 1}]}),
            "`x` must be a number",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "color": "red"}]}),
            "`color` must be \"1\" to \"6\"",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "ref": "a b"}]}),
            "`ref` must be 1 to 64",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "ref": "r"}, {"op": "add_edge", "ref": "r", "from": "a", "to": "f"}]}),
            "the ref r is given twice",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_edge", "from": "a", "to": "f", "toSide": "up"}]}),
            "`toSide` must be one of top, right, bottom, left",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_edge", "from": "a", "to": "f", "toEnd": "dot"}]}),
            "`toEnd` must be one of none, arrow",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_edge", "from": "a"}]}),
            "`to` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a"}]}),
            "nothing to change",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "x": null}]}),
            "`x` must be a number",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "text": null}]}),
            "`text` cannot be removed",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "replace": {"old": "a"}}]}),
            "`new` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "replace": "a"}]}),
            "`replace` must be an object",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "x": 1, "from": "a"}]}),
            "`from` is not a field of update_node",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "update_edge", "id": "af"}]}),
            "nothing to change",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "delete_node"}]}),
            "`id` is required",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "delete_node", "id": "a", "x": 1}]}),
            "`x` is not a field of delete_node",
        ),
        (
            json!({"path": "a.canvas", "ops": [{"op": "to_back", "id": ""}]}),
            "`id` must not be empty",
        ),
        // Checked against the canvas: a ref that is an id already.
        (
            json!({"path": "a.canvas", "ops": [{"op": "add_node", "type": "text", "text": "x", "ref": "a"}]}),
            "the ref a is the id of a node",
        ),
    ] {
        let text = place.refused("canvas_edit", arguments.clone(), "invalid");
        assert!(text.contains(says), "{arguments}: {text}");
    }
    assert_eq!(
        fs::read_to_string(place.work().join("a.canvas")).unwrap(),
        THREE
    );
}

#[test]
fn if_revision_refuses_a_canvas_changed_since_as_stale() {
    let place = Place::new("stale");
    place.write("work/a.canvas", THREE);
    let revision = place.output(&[], json!({"path": "a.canvas"}))["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let op = json!([{"op": "update_node", "id": "a", "x": 1}]);
    let output = place.done(
        "canvas_edit",
        json!({"path": "a.canvas", "ops": op, "if_revision": revision}),
    );
    let now = output["revision"].as_str().unwrap().to_owned();
    assert_ne!(now, revision);
    // The revision read before that write is stale now.
    let before = fs::read(place.work().join("a.canvas")).unwrap();
    let text = place.refused(
        "canvas_edit",
        json!({"path": "a.canvas", "ops": [{"op": "update_node", "id": "a", "x": 2}], "if_revision": revision}),
        "stale",
    );
    assert!(text.contains(&now), "the current revision: {text}");
    assert!(text.contains("Nothing was applied"), "{text}");
    assert_eq!(fs::read(place.work().join("a.canvas")).unwrap(), before);
}

/// The lock a canvas's writes take: `canvas-<FNV-1a 64 of its canonical path>.lock` in the
/// runtime folder's `rature`.
fn lock_of(place: &Place, canvas: &Path) -> PathBuf {
    let hash = canvas
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    place
        .root
        .join("xdg/runtime/rature")
        .join(format!("canvas-{hash:016x}.lock"))
}

#[test]
fn a_canvas_another_process_is_writing_is_busy() {
    let place = Place::new("busy");
    let file = place.write("work/a.canvas", THREE);
    let lock = lock_of(&place, &file);
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let held = fs::File::create(&lock).unwrap();
    held.lock().unwrap();
    // A new canvas's lock is its path's.
    let new = fs::File::create(lock_of(&place, &place.work().join("new.canvas"))).unwrap();
    new.lock().unwrap();
    let started = Instant::now();
    let text = place.edit_refused(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "x": 1}]),
        "busy",
    );
    assert!(started.elapsed() >= Duration::from_secs(2), "waited for it");
    assert!(text.contains("Try again"), "{text}");
    let text = place.refused("canvas_create", json!({"path": "new.canvas"}), "busy");
    assert!(text.contains("new.canvas"), "{text}");
    assert!(!place.work().join("new.canvas").exists());
    drop((held, new));
    place.edit(
        "a.canvas",
        json!([{"op": "update_node", "id": "a", "x": 1}]),
    );
    place.done("canvas_create", json!({"path": "new.canvas"}));
}

#[test]
fn a_canvas_with_flaws_or_not_json_is_never_written() {
    let place = Place::new("edit-broken");
    place.write(
        "work/twice.canvas",
        r#"{"nodes":[{"id":"a","type":"text","text":"x"},{"id":"a","type":"text","text":"y"}],"edges":[]}"#,
    );
    let text = place.edit_refused(
        "twice.canvas",
        json!([{"op": "add_node", "type": "text", "text": "z"}]),
        "not_a_canvas",
    );
    assert!(text.contains("the node id a is already"), "{text}");
    place.write("work/broken.canvas", "{\"nodes\":[");
    let text = place.edit_refused(
        "broken.canvas",
        json!([{"op": "add_node", "type": "text", "text": "z"}]),
        "not_a_canvas",
    );
    assert!(text.contains("not JSON"), "{text}");
    fs::write(
        place.work().join("latin1.canvas"),
        b"{\"nodes\":[],\"x\":\"\xe9\"}",
    )
    .unwrap();
    let text = place.edit_refused(
        "latin1.canvas",
        json!([{"op": "add_node", "type": "text", "text": "z"}]),
        "not_a_canvas",
    );
    assert!(text.contains("UTF-8"), "{text}");
}

#[test]
fn the_writing_tools_stay_in_the_allowed_folders() {
    let place = Place::new("write-outside");
    let out = place.write("elsewhere/out.canvas", THREE);
    std::os::unix::fs::symlink(&out, place.work().join("link.canvas")).unwrap();
    std::os::unix::fs::symlink(place.elsewhere(), place.work().join("linked")).unwrap();
    let op = json!([{"op": "update_node", "id": "a", "x": 1}]);
    for path in [
        out.to_str().unwrap(),
        "../elsewhere/out.canvas",
        "link.canvas",
        "linked/out.canvas",
    ] {
        let text = place.refused("canvas_edit", json!({"path": path, "ops": op}), "outside");
        assert!(text.contains("--allow-dir"), "{path}: {text}");
    }
    for path in [
        "../elsewhere/new.canvas",
        "linked/new.canvas",
        "../elsewhere/none/new.canvas",
    ] {
        place.refused("canvas_create", json!({"path": path}), "outside");
    }
    assert_eq!(fs::read_to_string(&out).unwrap(), THREE);
    assert!(!place.elsewhere().join("new.canvas").exists());
    // Allowed, it is written, through the link too.
    let elsewhere = place.elsewhere();
    let allow = ["--allow-dir", elsewhere.to_str().unwrap()];
    let result = place.call(
        &allow,
        "canvas_edit",
        json!({"path": "link.canvas", "ops": op}),
    );
    assert_eq!(result["isError"], false, "{result}");
    assert!(
        fs::symlink_metadata(place.work().join("link.canvas"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_ne!(fs::read_to_string(&out).unwrap(), THREE);
    let result = place.call(
        &allow,
        "canvas_create",
        json!({"path": "linked/new.canvas"}),
    );
    assert_eq!(result["isError"], false, "{result}");
    assert!(place.elsewhere().join("new.canvas").is_file());
}

#[test]
fn canvas_create_makes_a_canvas_with_its_cards_placed_apart() {
    let place = Place::new("create");
    let output = place.done(
        "canvas_create",
        json!({
            "path": "plan.canvas",
            "nodes": [
                {"ref": "idea", "type": "text", "text": "# Idea\n\nSomething worth a card of its own, with a few lines of text to make it taller than the others."},
                {"ref": "why", "type": "text", "text": "Why", "near": "idea"},
                {"ref": "how", "type": "text", "text": "How", "near": "idea"},
                {"ref": "doc", "type": "file", "file": "notes.md"},
                {"op": "add_node", "type": "link", "url": "https://example.org", "near": "why"}
            ],
            "edges": [
                {"ref": "e1", "from": "idea", "to": "why"},
                {"from": "idea", "to": "how", "toSide": "left"}
            ]
        }),
    );
    assert_eq!(output["changed"], json!([]));
    assert_eq!(output["deleted"], json!([]));
    let created: Vec<&str> = output["created"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        created,
        ["idea", "why", "how", "doc", "nodes[4]", "e1", "edges[1]"]
    );
    let canvas = place.canvas("plan.canvas");
    assert_eq!(canvas["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(canvas["edges"].as_array().unwrap().len(), 2);
    assert_apart(&canvas);
    let idea = by_id(
        &canvas["nodes"],
        output["created"]["idea"].as_str().unwrap(),
    );
    assert_eq!((idea["x"].clone(), idea["y"].clone()), (json!(0), json!(0)));
    let read = place.output(&[], json!({"path": "plan.canvas"}));
    assert_eq!(read["revision"], output["revision"]);
    assert!(read.get("problems").is_none());

    // An empty one.
    let output = place.done("canvas_create", json!({"path": "empty.canvas"}));
    assert_eq!(output["created"], json!({}));
    assert_eq!(
        fs::read_to_string(place.work().join("empty.canvas")).unwrap(),
        "{\n\t\"nodes\":[],\n\t\"edges\":[]\n}"
    );
}

#[test]
fn canvas_create_never_writes_over_a_file_nor_makes_a_folder() {
    let place = Place::new("create-refused");
    place.write("work/a.canvas", THREE);
    let text = place.refused("canvas_create", json!({"path": "a.canvas"}), "exists");
    assert!(text.contains("canvas_edit"), "{text}");
    std::os::unix::fs::symlink("nowhere.canvas", place.work().join("dangling.canvas")).unwrap();
    place.refused(
        "canvas_create",
        json!({"path": "dangling.canvas"}),
        "exists",
    );
    assert!(!place.work().join("nowhere.canvas").exists());
    assert_eq!(
        fs::read_to_string(place.work().join("a.canvas")).unwrap(),
        THREE
    );

    let text = place.refused("canvas_create", json!({"path": "new/plan.canvas"}), "io");
    assert!(text.contains("makes no folder"), "{text}");
    assert!(!place.work().join("new").exists());

    for (arguments, says) in [
        (json!({"path": "plan.md"}), ".canvas file"),
        (json!({"path": "a.canvas/x.canvas"}), "not a folder"),
        (
            json!({"path": "p.canvas", "nodes": {}}),
            "`nodes` must be a list",
        ),
        (
            json!({"path": "p.canvas", "nodes": [{"op": "update_node", "id": "a"}]}),
            "nodes[0]: an entry of `nodes` is an add_node",
        ),
        (
            json!({"path": "p.canvas", "nodes": [{"type": "text"}]}),
            "nodes[0]: `text` is required",
        ),
        (
            json!({"path": "p.canvas", "edges": [{"from": "x", "to": "y", "bend": 1}]}),
            "edges[0]: `bend` is not a field of add_edge",
        ),
        (
            json!({"path": "p.canvas", "ops": []}),
            "`ops` is not an argument of canvas_create",
        ),
    ] {
        let text = place.refused("canvas_create", arguments.clone(), "invalid");
        assert!(text.contains(says), "{arguments}: {text}");
    }
    let text = place.refused(
        "canvas_create",
        json!({"path": "p.canvas", "nodes": [{"ref": "a", "type": "text", "text": "A"}], "edges": [{"from": "a", "to": "b"}]}),
        "not_found",
    );
    assert!(text.starts_with("edges[0]: `to`"), "{text}");
    assert!(!place.work().join("p.canvas").exists());
}

#[test]
fn cards_placed_by_rature_through_canvas_edit_never_overlap() {
    let place = Place::new("placement");
    place.write("work/a.canvas", SAMPLE);
    let texts = [
        "Short",
        "A card with a longer line of text, long enough to be wrapped once or twice.",
        "# Title\n\n- one\n- two\n- three\n- four",
        "",
    ];
    // In several calls, as a client adds cards one after the other.
    for round in 0..4 {
        let mut ops = Vec::new();
        for (n, text) in texts.iter().enumerate() {
            let mut op =
                json!({"op": "add_node", "ref": format!("c{n}"), "type": "text", "text": text});
            match (round + n) % 3 {
                0 => op["near"] = json!("59e896bc8da20699"),
                1 if n > 0 => op["near"] = json!(format!("c{}", n - 1)),
                _ => {}
            }
            ops.push(op);
        }
        ops.push(json!({"op": "add_node", "type": "file", "file": "readme.md", "near": "8132d4d894c80022"}));
        ops.push(json!({"op": "add_node", "type": "group", "label": "Later", "near": "754a8ef995f366bc"}));
        place.edit("a.canvas", Value::Array(ops));
    }
    let canvas = place.canvas("a.canvas");
    assert_eq!(canvas["nodes"].as_array().unwrap().len(), 5 + 4 * 6);
    // The sample's own cards do not overlap, but its group holds three: leave them out.
    let mut placed = canvas.clone();
    placed["nodes"] = canvas["nodes"].as_array().unwrap()[1..].to_vec().into();
    assert_apart(&placed);
}
