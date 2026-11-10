//! What a file is shown as: Markdown, data (JSON, YAML) as a tree that folds, a table (CSV), an
//! image, or code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::document::{self, Document};
use crate::{fsio, icons};
use gpui_design_system::Icon;

/// Larger than this, a file is not read: Rature is for reading, not for logs.
const LARGEST: u64 = 10 * 1024 * 1024;

pub enum Content {
    /// Shared with the work done off the main thread: "what changed" compares it there.
    Markdown(Arc<Document>),
    Code {
        lang: String,
        text: String,
    },
    Data(Vec<Node>),
    Table(Vec<Vec<String>>),
    Image(PathBuf),
    /// No file open.
    Empty,
    /// Why the file is not shown.
    Message(String),
}

pub struct Node {
    pub key: Option<String>,
    pub value: Value,
}

pub enum Value {
    Scalar(String, Kind),
    List(Vec<Node>),
    Map(Vec<Node>),
}

#[derive(Clone, Copy)]
pub enum Kind {
    Text,
    Number,
    Bool,
    Null,
}

const IMAGES: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"];

pub fn is_markdown(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "md" | "markdown" | "mdx" | "mdown"
    )
}

/// The icon a file or a folder is listed with. It looks at the disk: drawing a file known to be
/// one takes `file_icon`.
pub fn icon(path: &Path) -> Icon {
    if path.is_dir() {
        return Icon::Folder;
    }
    file_icon(path)
}

/// The icon a file is listed with, by its name alone.
pub fn file_icon(path: &Path) -> Icon {
    let ext = extension(path);
    match ext.as_str() {
        _ if is_markdown(path) => Icon::Text,
        "json" | "yaml" | "yml" | "toml" => icons::DATA,
        "csv" | "tsv" => icons::TABLE,
        _ if IMAGES.contains(&ext.as_str()) => icons::IMAGE,
        "" | "txt" | "log" => Icon::File,
        _ => icons::CODE,
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// A file read, and whether Rature may write it back.
pub struct Loaded {
    pub content: Content,
    /// Its text is UTF-8: what is shown is what it holds. Text that is not is shown with its
    /// bad bytes replaced, and never edited, so it is never written back with them lost.
    pub editable: bool,
}

impl Loaded {
    fn shown(content: Content) -> Self {
        Self {
            content,
            editable: false,
        }
    }
}

/// Reads the file at `path`, as its kind says. Formulas are drawn in `ink`. Only a regular file
/// is read: a pipe or a device would hold the window waiting. It reads, parses and draws: call it
/// off the main thread.
pub fn load(path: &Path, ink: &str) -> std::io::Result<Loaded> {
    use std::io::Read as _;
    if let Some(what) = fsio::not_a_file(&std::fs::metadata(path)?) {
        return Ok(Loaded::shown(not_read(what)));
    }
    let ext = extension(path);
    if IMAGES.contains(&ext.as_str()) {
        return Ok(Loaded::shown(Content::Image(path.to_path_buf())));
    }
    let file = std::fs::File::open(path)?;
    // Looked at again through the file opened: the path may have changed meanwhile.
    let meta = file.metadata()?;
    if let Some(what) = fsio::not_a_file(&meta) {
        return Ok(Loaded::shown(not_read(what)));
    }
    let too_large = |size: u64| {
        Loaded::shown(Content::Message(format!(
            "It is {} MB: too large to read here. e opens it in nvim.",
            size / 1024 / 1024
        )))
    };
    if meta.len() > LARGEST {
        return Ok(too_large(meta.len()));
    }
    // No more than the limit, even from a file that grows while it is read.
    let mut bytes = Vec::new();
    file.take(LARGEST + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LARGEST {
        return Ok(too_large(bytes.len() as u64));
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok(Loaded::shown(Content::Message("It is not text.".into())));
    }
    let (text, editable) = match String::from_utf8(bytes) {
        Ok(text) => (text, true),
        Err(error) => (
            String::from_utf8_lossy(error.as_bytes()).into_owned(),
            false,
        ),
    };
    Ok(Loaded {
        content: read(path, &ext, text, ink),
        editable,
    })
}

fn not_read(what: &str) -> Content {
    Content::Message(format!("It is {what}: only regular files are read here."))
}

fn read(path: &Path, ext: &str, text: String, ink: &str) -> Content {
    match ext {
        "md" | "markdown" | "mdx" | "mdown" => {
            Content::Markdown(Arc::new(document::parse(&text, ink, path.parent())))
        }
        "json" => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => Content::Data(vec![from_json(None, value)]),
            Err(_) => code("json", text),
        },
        "yaml" | "yml" => match yaml_rust2::YamlLoader::load_from_str(&text) {
            Ok(docs) if !docs.is_empty() => {
                Content::Data(docs.into_iter().map(|d| from_yaml(None, d)).collect())
            }
            _ => code("yaml", text),
        },
        "csv" | "tsv" => {
            let delimiter = if ext == "tsv" { b'\t' } else { b',' };
            let rows: Result<Vec<Vec<String>>, _> = csv::ReaderBuilder::new()
                .delimiter(delimiter)
                .has_headers(false)
                .flexible(true)
                .from_reader(text.as_bytes())
                .records()
                .map(|r| r.map(|r| r.iter().map(str::to_string).collect()))
                .collect();
            match rows {
                Ok(rows) => Content::Table(rows),
                Err(_) => code(ext, text),
            }
        }
        _ => {
            let lang = if ext.is_empty() {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                ext.to_string()
            };
            code(&lang, text)
        }
    }
}

fn code(lang: &str, text: String) -> Content {
    Content::Code {
        lang: lang.to_string(),
        text,
    }
}

fn from_json(key: Option<String>, value: serde_json::Value) -> Node {
    use serde_json::Value as J;
    let value = match value {
        J::Null => Value::Scalar("null".into(), Kind::Null),
        J::Bool(b) => Value::Scalar(b.to_string(), Kind::Bool),
        J::Number(n) => Value::Scalar(n.to_string(), Kind::Number),
        J::String(s) => Value::Scalar(s, Kind::Text),
        J::Array(items) => Value::List(items.into_iter().map(|v| from_json(None, v)).collect()),
        J::Object(map) => Value::Map(
            map.into_iter()
                .map(|(k, v)| from_json(Some(k), v))
                .collect(),
        ),
    };
    Node { key, value }
}

fn from_yaml(key: Option<String>, value: yaml_rust2::Yaml) -> Node {
    use yaml_rust2::Yaml as Y;
    let value = match value {
        Y::Null | Y::BadValue => Value::Scalar("null".into(), Kind::Null),
        Y::Boolean(b) => Value::Scalar(b.to_string(), Kind::Bool),
        Y::Integer(i) => Value::Scalar(i.to_string(), Kind::Number),
        Y::Real(r) => Value::Scalar(r, Kind::Number),
        Y::String(s) => Value::Scalar(s, Kind::Text),
        Y::Alias(a) => Value::Scalar(format!("*{a}"), Kind::Text),
        Y::Array(items) => Value::List(items.into_iter().map(|v| from_yaml(None, v)).collect()),
        Y::Hash(hash) => Value::Map(
            hash.into_iter()
                .map(|(k, v)| {
                    let key = match k {
                        Y::String(s) | Y::Real(s) => s,
                        Y::Integer(i) => i.to_string(),
                        Y::Boolean(b) => b.to_string(),
                        _ => "?".into(),
                    };
                    from_yaml(Some(key), v)
                })
                .collect(),
        ),
    };
    Node { key, value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str, text: &str) -> Content {
        let path = Path::new(name);
        read(path, &extension(path), text.to_string(), "#ffffff")
    }

    #[test]
    fn each_kind_of_file_is_shown_as_its_kind() {
        assert!(matches!(kind("a.md", "# A\n"), Content::Markdown(_)));
        assert!(matches!(
            kind("a.json", "{\"a\": [1, 2]}"),
            Content::Data(_)
        ));
        assert!(matches!(kind("a.json", "{broken"), Content::Code { .. }));
        assert!(matches!(kind("a.yaml", "a:\n  b: 1\n"), Content::Data(_)));
        assert!(matches!(kind("a.rs", "fn main() {}"), Content::Code { lang, .. } if lang == "rs"));
        let Content::Table(rows) = kind("a.csv", "herb,grams\nbasil,120\n") else {
            panic!("not a table")
        };
        assert_eq!(rows[1], ["basil", "120"]);
    }

    fn folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("rature-formats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn text_that_is_not_utf8_is_shown_but_not_editable() {
        let folder = folder("utf8");
        let bad = folder.join("a.rs");
        std::fs::write(&bad, b"// caf\xe9\nfn main() {}\n").unwrap();
        let loaded = load(&bad, "#ffffff").unwrap();
        assert!(!loaded.editable);
        let Content::Code { text, .. } = loaded.content else {
            panic!("not code")
        };
        assert!(text.contains("caf\u{fffd}"));
        let good = folder.join("b.md");
        std::fs::write(&good, "# Café ü 🎉\n").unwrap();
        assert!(load(&good, "#ffffff").unwrap().editable);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_pipe_or_a_device_is_not_read() {
        let folder = folder("pipe");
        let pipe = folder.join("p.md");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        // Read, it would wait for a writer forever: this returns at once.
        let loaded = load(&pipe, "#ffffff").unwrap();
        assert!(matches!(loaded.content, Content::Message(m) if m.contains("named pipe")));
        let loaded = load(Path::new("/dev/zero"), "#ffffff").unwrap();
        assert!(matches!(loaded.content, Content::Message(m) if m.contains("device")));
        assert!(!loaded.editable);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn json_keeps_the_order_of_its_keys() {
        let Content::Data(nodes) = kind("a.json", "{\"z\": 1, \"a\": 2}") else {
            panic!("not data")
        };
        let Value::Map(entries) = &nodes[0].value else {
            panic!("not a map")
        };
        let keys: Vec<&str> = entries.iter().filter_map(|n| n.key.as_deref()).collect();
        assert_eq!(keys, ["z", "a"]);
    }
}
