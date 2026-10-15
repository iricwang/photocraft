//! Bounded, lossless Defold TextProto editing. Unknown fields are retained verbatim.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::Serialize;
use std::ops::Range;

pub type Result<T> = std::result::Result<T, String>;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Field {
    pub span: Range<usize>,
    pub name: String,
    pub value: Range<usize>,
    pub children: Vec<Field>,
    pub message: bool,
}
#[derive(Clone, Debug)]
pub struct TextProto {
    pub source: String,
    pub fields: Vec<Field>,
}
#[derive(Clone)]
struct Token {
    text: String,
    span: Range<usize>,
}

fn tokens(s: &str) -> Result<Vec<Token>> {
    if s.len() > MAX_BYTES {
        return Err("Defold resource exceeds 16 MiB".into());
    }
    let b = s.as_bytes();
    let mut i = if s.starts_with('\u{feff}') { 3 } else { 0 };
    let mut out = Vec::new();
    while let Some(&c) = b.get(i) {
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'#' || (c == b'/' && b.get(i + 1) == Some(&b'/')) {
            while b.get(i).is_some_and(|c| *c != b'\n') {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b.get(i) == Some(&b'*') && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            if i == b.len() {
                return Err("Unterminated comment".into());
            }
            i += 2;
            continue;
        }
        let start = i;
        if c == b'\"' || c == b'\'' {
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err("Unterminated string".into()),
                    Some(x) if *x == c => {
                        i += 1;
                        break;
                    }
                    Some(b'\\') => {
                        i += 2;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
        } else if b"{}<>:[],;".contains(&c) {
            i += 1;
        } else {
            while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !b"{}<>:[],;#\"'".contains(c)) {
                i += 1;
            }
        }
        out.push(Token { text: s.get(start..i).ok_or("Invalid token")?.into(), span: start..i });
        if out.len() > 500_000 {
            return Err("Too many TextProto tokens".into());
        }
    }
    Ok(out)
}
fn fields(ts: &[Token], at: &mut usize, end: Option<&str>, depth: usize) -> Result<Vec<Field>> {
    if depth > 64 {
        return Err("TextProto nesting exceeds 64".into());
    }
    let mut out = Vec::new();
    while let Some(t) = ts.get(*at) {
        if Some(t.text.as_str()) == end {
            *at += 1;
            return Ok(out);
        }
        if t.text == ";" || t.text == "," {
            *at += 1;
            continue;
        }
        let name = t.text.clone();
        if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(format!("Invalid field {name}"));
        }
        *at += 1;
        if ts.get(*at).is_some_and(|t| t.text == ":") {
            *at += 1;
        }
        let v = ts.get(*at).ok_or("Missing field value")?;
        let start = v.span.start;
        let message = v.text == "{" || v.text == "<";
        let mut children = Vec::new();
        if message {
            let close = if v.text == "{" { "}" } else { ">" };
            *at += 1;
            children = fields(ts, at, Some(close), depth + 1)?;
        } else if v.text == "[" {
            let mut nesting = 1;
            *at += 1;
            while nesting > 0 {
                let t = ts.get(*at).ok_or("Unterminated list")?;
                if t.text == "[" {
                    nesting += 1;
                }
                if t.text == "]" {
                    nesting -= 1;
                }
                if nesting > 64 {
                    return Err("List nesting exceeds 64".into());
                }
                *at += 1;
            }
        } else {
            if matches!(v.text.as_str(), "}" | ">" | ":" | "]") {
                return Err("Invalid scalar".into());
            }
            let string = v.text.starts_with(['\"', '\'']);
            *at += 1;
            if string {
                while ts.get(*at).is_some_and(|t| t.text.starts_with(['\"', '\''])) {
                    *at += 1;
                }
            }
        }
        let finish = ts.get(at.saturating_sub(1)).ok_or("Missing value")?.span.end;
        out.push(Field { name, span: t.span.start..finish, value: start..finish, children, message });
    }
    if end.is_some() {
        return Err("Unterminated message".into());
    }
    Ok(out)
}
impl TextProto {
    pub fn parse(source: String) -> Result<Self> {
        let ts = tokens(&source)?;
        let fields = fields(&ts, &mut 0, None, 0)?;
        Ok(Self { source, fields })
    }
    pub fn raw<'a>(&'a self, f: &Field) -> &'a str {
        self.source.get(f.value.clone()).unwrap_or("")
    }
    pub fn string(&self, f: &Field) -> Result<String> {
        let mut bytes = Vec::new();
        for t in tokens(self.raw(f))? {
            if let Ok(decoded) = serde_json::from_str::<String>(&t.text) {
                bytes.extend_from_slice(decoded.as_bytes());
                continue;
            }
            let b = t.text.as_bytes();
            if !t.text.starts_with(['\"', '\'']) {
                return Err("Expected quoted string".into());
            }
            let mut i = 1;
            while i + 1 < b.len() {
                let c = *b.get(i).ok_or("Invalid string")?;
                i += 1;
                if c != b'\\' {
                    bytes.push(c);
                    continue;
                }
                let c = *b.get(i).ok_or("Invalid escape")?;
                i += 1;
                match c {
                    b'n' => bytes.push(b'\n'),
                    b'r' => bytes.push(b'\r'),
                    b't' => bytes.push(b'\t'),
                    b'a' => bytes.push(7),
                    b'b' => bytes.push(8),
                    b'f' => bytes.push(12),
                    b'v' => bytes.push(11),
                    b'\\' | b'\"' | b'\'' | b'?' => bytes.push(c),
                    b'0'..=b'7' => {
                        let mut n = u16::from(c - b'0');
                        for _ in 0..2 {
                            if let Some(c @ b'0'..=b'7') = b.get(i) {
                                n = n * 8 + u16::from(c - b'0');
                                i += 1;
                            } else {
                                break;
                            }
                        }
                        bytes.push(u8::try_from(n).map_err(|_| "Invalid octal escape")?);
                    }
                    b'x' => {
                        let h = t.text.get(i..i + 2).ok_or("Invalid hex escape")?;
                        bytes.push(u8::from_str_radix(h, 16).map_err(|_| "Invalid hex escape")?);
                        i += 2;
                    }
                    _ => return Err("Unsupported string escape".into()),
                }
            }
        }
        String::from_utf8(bytes).map_err(|_| "String is not UTF-8".into())
    }
    pub fn text(&self, fs: &[Field], key: &str) -> String {
        fs.iter().find(|f| f.name == key).and_then(|f| self.string(f).ok()).unwrap_or_default()
    }
    pub fn scalar<'a>(&'a self, fs: &[Field], key: &str, default: &'a str) -> &'a str {
        fs.iter().find(|f| f.name == key).map_or(default, |f| self.raw(f))
    }
    fn patch(&self, parent: &Field, path: &[&str], value: &str) -> Result<Self> {
        let key = path.first().ok_or("Empty property path")?;
        let matches: Vec<_> = parent.children.iter().filter(|f| f.name == *key).collect();
        if matches.len() > 1 {
            return Err("Ambiguous duplicate property".into());
        }
        if let Some(field) = matches.first() {
            if path.len() > 1 {
                return self.patch(field, path.get(1..).ok_or("Invalid path")?, value);
            }
            let mut source = self.source.clone();
            source.replace_range(field.value.clone(), value);
            Self::parse(source)
        } else {
            let mut insertion = value.to_string();
            for (i, key) in path.iter().enumerate().rev() {
                insertion = if i + 1 == path.len() { format!("{key}: {insertion}") } else { format!("{key} {{ {insertion} }}") };
            }
            let pos = parent.value.end.checked_sub(1).ok_or("Invalid message")?;
            if !parent.message {
                return Err("Expected message".into());
            }
            let mut source = self.source.clone();
            source.insert_str(pos, &format!("\n  {insertion}\n"));
            Self::parse(source)
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Node {
    pub id: String,
    pub parent: String,
    pub kind: String,
    pub text: String,
    pub texture: String,
    pub position: [f64; 3],
    pub size: [f64; 3],
    pub scale: [f64; 3],
    pub rotation: [f64; 3],
    pub color: [f64; 4],
    pub alpha: f64,
    pub visible: bool,
    pub enabled: bool,
    pub pivot: String,
    pub editable: bool,
}
#[derive(Clone, Debug)]
pub struct Scene {
    pub proto: TextProto,
    pub nodes: Vec<Node>,
}
mod editing;
pub use editing::{Property, PropertyKind, properties};
impl Scene {
    pub fn parse(source: String) -> Result<Self> {
        let proto = TextProto::parse(source)?;
        let mut nodes = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for f in proto.fields.iter().filter(|f| f.name == "nodes") {
            if nodes.len() >= 8192 {
                return Err("GUI exceeds 8192 nodes".into());
            }
            let fs = &f.children;
            let id = proto.text(fs, "id");
            if id.is_empty() || !ids.insert(id.clone()) {
                return Err("Missing or duplicate GUI node id".into());
            }
            let kind = proto.scalar(fs, "type", "TYPE_BOX").to_string();
            let vector = |key, default: [f64; 3]| -> Result<[f64; 3]> {
                let child = fs.iter().find(|f| f.name == key).map_or(&[][..], |f| f.children.as_slice());
                let mut v = default;
                for (i, k) in ["x", "y", "z"].iter().enumerate() {
                    if let Some(f) = child.iter().find(|f| f.name == *k) {
                        v[i] = number(proto.raw(f))?;
                    }
                }
                Ok(v)
            };
            let color_fields = fs.iter().find(|f| f.name == "color").map_or(&[][..], |f| f.children.as_slice());
            let mut color = [1.0; 4];
            for (i, k) in ["x", "y", "z", "w"].iter().enumerate() {
                color[i] = number(proto.scalar(color_fields, k, "1"))?;
            }
            nodes.push(Node {
                id,
                parent: proto.text(fs, "parent"),
                kind: kind.clone(),
                text: proto.text(fs, "text"),
                texture: proto.text(fs, "texture"),
                position: vector("position", [0.0; 3])?,
                size: vector("size", [0.0; 3])?,
                scale: vector("scale", [1.0; 3])?,
                rotation: vector("rotation", [0.0; 3])?,
                color,
                alpha: number(proto.scalar(fs, "alpha", "1"))?,
                visible: proto.scalar(fs, "visible", "true") != "false",
                enabled: proto.scalar(fs, "enabled", "true") != "false",
                pivot: proto.scalar(fs, "pivot", "PIVOT_CENTER").into(),
                editable: matches!(kind.as_str(), "TYPE_BOX" | "TYPE_TEXT" | "TYPE_PIE" | "TYPE_PARTICLEFX" | "TYPE_TEMPLATE")
                    && proto.scalar(fs, "template_node_child", "false") != "true",
            });
        }
        let by_id: std::collections::HashMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        for node in &nodes {
            let mut seen = std::collections::HashSet::new();
            let mut parent = node;
            while !parent.parent.is_empty() {
                if !seen.insert(parent.id.clone()) || seen.len() > 256 {
                    return Err("Cyclic or overly deep GUI hierarchy".into());
                }
                parent = by_id.get(parent.parent.as_str()).copied().ok_or("GUI parent is missing")?;
            }
        }
        Ok(Self { proto, nodes })
    }
    pub fn edit(&self, id: &str, property: &str, value: &serde_json::Value) -> Result<Self> {
        let n = self.nodes.iter().find(|n| n.id == id).ok_or("Unknown GUI node")?;
        if !n.editable {
            return Err("Template/custom node is preserved read-only".into());
        }
        let encoded = editing::encode(&n.kind, property, value)?;
        let f = self.proto.fields.iter().find(|f| f.name == "nodes" && self.proto.text(&f.children, "id") == id).ok_or("Missing node source")?;
        let proto = self.proto.patch(f, &property.split('.').collect::<Vec<_>>(), &encoded)?;
        Self::parse(proto.source)
    }
}
fn number(s: &str) -> Result<f64> {
    let v: f64 = s.parse().map_err(|_| "Invalid GUI number")?;
    if v.is_finite() && v.abs() <= 1_000_000.0 { Ok(v) } else { Err("GUI number is out of range".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lossless_and_surgical_patch() {
        let source = "# preserved\r\nnodes { id: \"a\" type: TYPE_TEXT text: \"你好\" future { v: 42 } position { x: 3 y: 2 } }\n";
        let s = Scene::parse(source.into()).unwrap();
        assert_eq!(s.proto.source, source);
        let s = s.edit("a", "position.x", &serde_json::json!(10)).unwrap();
        assert_eq!(s.proto.source, source.replace("x: 3", "x: 10"));
        let s = s.edit("a", "size.y", &serde_json::json!(25)).unwrap();
        assert_eq!(s.nodes[0].size[1], 25.0);
        assert_eq!(s.nodes[0].text, "你好");
    }
    #[test]
    fn reject_ambiguous_or_hostile_input() {
        for s in ["nodes { id: \"a\" } nodes { id: \"a\" }", "nodes { id: \"a\" parent: \"a\" }", "nodes { id: \"a\" position { x: nan } }", "nodes {"] {
            assert!(Scene::parse(s.into()).is_err());
        }
        let s = Scene::parse("nodes { id: \"a\" position { x: 1 x: 2 } }".into()).unwrap();
        assert!(s.edit("a", "position.x", &serde_json::json!(3)).is_err());
    }
    #[test]
    fn protobuf_strings_and_unknown_lists() {
        let p = TextProto::parse("name: '\\344\\275\\240' '好' unknown: [1, 2, 3]".into()).unwrap();
        assert_eq!(p.text(&p.fields, "name"), "你好");
    }
}
