//! What a file is shown as: Markdown, data (JSON, YAML) as a tree that folds, a table (CSV), an
//! image, or code.

use std::path::{Path, PathBuf};

use crate::document::{self, Document};

/// Larger than this, a file is not read: Rature is for reading, not for logs.
const LARGEST: u64 = 10 * 1024 * 1024;

pub enum Content {
    Markdown(Document),
    Code {
        lang: String,
        text: String,
    },
    Data(Vec<Node>),
    Table(Vec<Vec<String>>),
    Image(PathBuf),
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

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Reads the file at `path`, as its kind says. Formulas are drawn in `ink`.
pub fn load(path: &Path, ink: &str) -> std::io::Result<Content> {
    let ext = extension(path);
    if IMAGES.contains(&ext.as_str()) {
        return Ok(Content::Image(path.to_path_buf()));
    }
    let size = std::fs::metadata(path)?.len();
    if size > LARGEST {
        return Ok(Content::Message(format!(
            "{} is {} MB: too large to read here. `e` opens it in nvim.",
            path.display(),
            size / 1024 / 1024
        )));
    }
    let bytes = std::fs::read(path)?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok(Content::Message(format!("{} is not text.", path.display())));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(read(path, &ext, text, ink))
}

fn read(path: &Path, ext: &str, text: String, ink: &str) -> Content {
    match ext {
        "md" | "markdown" | "mdx" | "mdown" => {
            Content::Markdown(document::parse(&text, ink, path.parent()))
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
        let Content::Table(rows) = kind("a.csv", "plant,seeds\nBasil,120\n") else {
            panic!("not a table")
        };
        assert_eq!(rows[1], ["Basil", "120"]);
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
