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
    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool["name"], "canvas_read");
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
fn the_server_ends_when_stdin_does() {
    let place = Place::new("eof");
    let (out, _, status) = place.run(&[], &[]);
    assert!(status.success());
    assert_eq!(out, "");
}
