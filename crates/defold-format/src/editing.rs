//! Authored GUI properties and structural edits. Serialized unknown data stays intact.
use super::*;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub enum PropertyKind {
    Number,
    Boolean,
    Text,
    Enum(&'static [&'static str]),
}
#[derive(Clone, Debug)]
pub struct Property {
    pub path: String,
    pub group: &'static str,
    pub kind: PropertyKind,
    pub default: Value,
}
pub fn properties(kind: &str) -> Vec<Property> {
    let mut out = Vec::new();
    for (group, field, components, default) in [
        ("Transform", "position", "xyz", 0.0),
        ("Transform", "rotation", "xyz", 0.0),
        ("Transform", "scale", "xyz", 1.0),
        ("Transform", "size", "xyz", 0.0),
        ("Appearance", "color", "xyzw", 1.0),
    ] {
        for c in components.chars() {
            out.push(Property { path: format!("{field}.{c}"), group, kind: PropertyKind::Number, default: json!(default) });
        }
    }
    out.push(Property { path: "alpha".into(), group: "Appearance", kind: PropertyKind::Number, default: json!(1.0) });
    for (field, default) in [("visible", true), ("enabled", true), ("inherit_alpha", false)] {
        out.push(Property { path: field.into(), group: "Appearance", kind: PropertyKind::Boolean, default: json!(default) });
    }
    for field in ["layer", "material"] {
        out.push(Property { path: field.into(), group: "Resources", kind: PropertyKind::Text, default: json!("") });
    }
    for (field, group, choices) in [
        ("blend_mode", "Appearance", &["BLEND_MODE_ALPHA", "BLEND_MODE_ADD", "BLEND_MODE_ADD_ALPHA", "BLEND_MODE_MULT", "BLEND_MODE_SCREEN"][..]),
        ("pivot", "Layout", &["PIVOT_CENTER", "PIVOT_N", "PIVOT_NE", "PIVOT_E", "PIVOT_SE", "PIVOT_S", "PIVOT_SW", "PIVOT_W", "PIVOT_NW"]),
        ("xanchor", "Layout", &["XANCHOR_NONE", "XANCHOR_LEFT", "XANCHOR_RIGHT"]),
        ("yanchor", "Layout", &["YANCHOR_NONE", "YANCHOR_TOP", "YANCHOR_BOTTOM"]),
        ("adjust_mode", "Layout", &["ADJUST_MODE_FIT", "ADJUST_MODE_ZOOM", "ADJUST_MODE_STRETCH"]),
    ] {
        out.push(Property { path: field.into(), group, kind: PropertyKind::Enum(choices), default: json!(choices.first().copied().unwrap_or_default()) });
    }
    if matches!(kind, "TYPE_BOX" | "TYPE_PIE") {
        out.push(Property { path: "texture".into(), group: "Resources", kind: PropertyKind::Text, default: json!("") });
        out.push(Property {
            path: "size_mode".into(),
            group: "Layout",
            kind: PropertyKind::Enum(&["SIZE_MODE_MANUAL", "SIZE_MODE_AUTO"]),
            default: json!("SIZE_MODE_MANUAL"),
        });
        out.push(Property {
            path: "clipping_mode".into(),
            group: "Clipping",
            kind: PropertyKind::Enum(&["CLIPPING_MODE_NONE", "CLIPPING_MODE_STENCIL"]),
            default: json!("CLIPPING_MODE_NONE"),
        });
        for (key, default) in [("clipping_visible", true), ("clipping_inverted", false)] {
            out.push(Property { path: key.into(), group: "Clipping", kind: PropertyKind::Boolean, default: json!(default) });
        }
    }
    if kind == "TYPE_BOX" {
        for c in "xyzw".chars() {
            out.push(Property { path: format!("slice9.{c}"), group: "Nine slice", kind: PropertyKind::Number, default: json!(0) });
        }
    }
    if kind == "TYPE_TEXT" {
        for key in ["text", "font"] {
            out.push(Property { path: key.into(), group: "Text", kind: PropertyKind::Text, default: json!("") });
        }
        out.push(Property { path: "line_break".into(), group: "Text", kind: PropertyKind::Boolean, default: json!(false) });
        for (key, default) in [("text_leading", 1.0), ("text_tracking", 0.0), ("outline_alpha", 1.0), ("shadow_alpha", 1.0)] {
            out.push(Property { path: key.into(), group: "Text", kind: PropertyKind::Number, default: json!(default) });
        }
        for key in ["outline", "shadow"] {
            for c in "xyzw".chars() {
                out.push(Property { path: format!("{key}.{c}"), group: "Text", kind: PropertyKind::Number, default: json!(if c == 'w' { 1 } else { 0 }) });
            }
        }
    }
    if kind == "TYPE_PIE" {
        out.push(Property {
            path: "outerBounds".into(),
            group: "Pie",
            kind: PropertyKind::Enum(&["PIEBOUNDS_ELLIPSE", "PIEBOUNDS_RECTANGLE"]),
            default: json!("PIEBOUNDS_ELLIPSE"),
        });
        for (key, default) in [("innerRadius", 0), ("perimeterVertices", 32), ("pieFillAngle", 360)] {
            out.push(Property { path: key.into(), group: "Pie", kind: PropertyKind::Number, default: json!(default) });
        }
    }
    for (node, field) in [("TYPE_TEMPLATE", "template"), ("TYPE_PARTICLEFX", "particlefx")] {
        if node == kind {
            out.push(Property { path: field.into(), group: "Resources", kind: PropertyKind::Text, default: json!("") });
        }
    }
    out
}
pub(super) fn encode(kind: &str, path: &str, value: &Value) -> Result<String> {
    let p = properties(kind).into_iter().find(|p| p.path == path).ok_or("Unsupported GUI property")?;
    match p.kind {
        PropertyKind::Boolean => Ok(value.as_bool().ok_or("Expected boolean")?.to_string()),
        PropertyKind::Text => {
            let s = value.as_str().ok_or("Expected string")?;
            if s.len() > 65536 {
                return Err("Text exceeds limit".into());
            }
            serde_json::to_string(s).map_err(|e| e.to_string())
        }
        PropertyKind::Enum(choices) => {
            let s = value.as_str().ok_or("Expected enum")?;
            if !choices.contains(&s) {
                return Err("Invalid enum value".into());
            }
            Ok(s.into())
        }
        PropertyKind::Number => {
            let n = value.as_f64().ok_or("Expected number")?;
            number(&n.to_string())?;
            if ((path.starts_with("size.") || path.starts_with("slice9.") || path == "innerRadius") && n < 0.0)
                || (path.ends_with("alpha") && !(0.0..=1.0).contains(&n))
                || (path == "perimeterVertices" && (!(3.0..=1000.0).contains(&n) || n.fract() != 0.0))
            {
                return Err("Property is out of range".into());
            }
            Ok(n.to_string())
        }
    }
}
impl Scene {
    fn node_field(&self, id: &str) -> Result<&Field> {
        self.proto.fields.iter().find(|f| f.name == "nodes" && self.proto.text(&f.children, "id") == id).ok_or_else(|| "Unknown GUI node".into())
    }
    pub fn property(&self, id: &str, path: &str) -> Result<Value> {
        let n = self.nodes.iter().find(|n| n.id == id).ok_or("Unknown GUI node")?;
        let p = properties(&n.kind).into_iter().find(|p| p.path == path).ok_or("Unsupported GUI property")?;
        let mut children = &self.node_field(id)?.children;
        let keys: Vec<_> = path.split('.').collect();
        for (i, key) in keys.iter().enumerate() {
            let Some(f) = children.iter().find(|f| f.name == *key) else { return Ok(p.default) };
            if i + 1 == keys.len() {
                return Ok(match p.kind {
                    PropertyKind::Text => json!(self.proto.string(f)?),
                    PropertyKind::Boolean => json!(self.proto.raw(f) == "true"),
                    PropertyKind::Number => json!(number(self.proto.raw(f))?),
                    PropertyKind::Enum(_) => json!(self.proto.raw(f)),
                });
            }
            children = &f.children;
        }
        Err("Invalid property".into())
    }
    pub fn new_node(&self, id: &str, kind: &str, parent: &str, after: Option<&str>) -> Result<Self> {
        valid_id(id)?;
        if self.nodes.iter().any(|n| n.id == id) {
            return Err("Node ID already exists".into());
        }
        if !matches!(kind, "TYPE_BOX" | "TYPE_TEXT" | "TYPE_PIE" | "TYPE_PARTICLEFX") {
            return Err("Unsupported new node type".into());
        }
        if !parent.is_empty() && !self.nodes.iter().any(|n| n.id == parent && n.editable && n.kind != "TYPE_TEMPLATE") {
            return Err("Parent is missing or read-only".into());
        }
        let q = |s: &str| serde_json::to_string(s).map_err(|e| e.to_string());
        let mut source = self.proto.source.clone();
        let pos = if let Some(after) = after {
            let a = self.nodes.iter().find(|n| n.id == after).ok_or("Missing insertion node")?;
            if a.parent != parent {
                return Err("Insertion node has a different parent".into());
            }
            self.node_field(after)?.span.start
        } else {
            source.len()
        };
        let text = if kind == "TYPE_TEXT" {
            format!(
                "text: \"Text\"\nfont: {}\n",
                q(&self.proto.fields.iter().find(|f| f.name == "fonts").map(|f| self.proto.text(&f.children, "name")).unwrap_or_default())?
            )
        } else {
            String::new()
        };
        source.insert_str(pos, &format!("\nnodes {{ id: {} type: {kind} parent: {} size {{ x: 100 y: 40 }} {text}}}\n", q(id)?, q(parent)?));
        Self::parse(source)
    }
    pub fn remove_node(&self, id: &str) -> Result<Self> {
        if !self.nodes.iter().any(|n| n.id == id && n.editable) {
            return Err("Node is missing or read-only".into());
        }
        let mut removed = HashSet::from([id.to_string()]);
        for _ in 0..256 {
            let before = removed.len();
            for n in &self.nodes {
                if removed.contains(&n.parent) {
                    removed.insert(n.id.clone());
                }
            }
            if before == removed.len() {
                break;
            }
        }
        let mut ranges = Vec::new();
        for f in &self.proto.fields {
            if f.name == "nodes" && removed.contains(&self.proto.text(&f.children, "id")) {
                ranges.push(f.span.clone());
            } else if f.name == "layouts" {
                for n in &f.children {
                    if n.name == "nodes" && removed.contains(&self.proto.text(&n.children, "id")) {
                        ranges.push(n.span.clone());
                    }
                }
            }
        }
        let mut source = self.proto.source.clone();
        ranges.sort_by_key(|r| r.start);
        for r in ranges.into_iter().rev() {
            source.replace_range(r, "");
        }
        Self::parse(source)
    }
    pub fn rename_node(&self, id: &str, new_id: &str) -> Result<Self> {
        valid_id(new_id)?;
        if id != new_id && self.nodes.iter().any(|n| n.id == new_id) {
            return Err("Node ID already exists".into());
        }
        if !self.nodes.iter().any(|n| n.id == id && n.editable && n.kind != "TYPE_TEMPLATE") {
            return Err("Node is missing or read-only".into());
        }
        let mut ranges = Vec::new();
        for f in &self.proto.fields {
            let fs: Vec<_> = if f.name == "nodes" {
                vec![f]
            } else if f.name == "layouts" {
                f.children.iter().filter(|c| c.name == "nodes").collect()
            } else {
                vec![]
            };
            for n in fs {
                for p in &n.children {
                    if matches!(p.name.as_str(), "id" | "parent") && self.proto.string(p).ok().as_deref() == Some(id) {
                        ranges.push(p.value.clone());
                    }
                }
            }
        }
        let q = serde_json::to_string(new_id).map_err(|e| e.to_string())?;
        let mut source = self.proto.source.clone();
        ranges.sort_by_key(|r| r.start);
        for r in ranges.into_iter().rev() {
            source.replace_range(r, &q);
        }
        Self::parse(source)
    }
    pub fn move_node(&self, id: &str, parent: &str, before: Option<&str>) -> Result<Self> {
        let node = self.nodes.iter().find(|n| n.id == id && n.editable).ok_or("Node is missing or read-only")?;
        if node.kind == "TYPE_TEMPLATE" {
            return Err("Template structural editing requires Defold".into());
        }
        let q = serde_json::to_string(parent).map_err(|e| e.to_string())?;
        let next = Self::parse(self.proto.patch(self.node_field(id)?, &["parent"], &q)?.source)?;
        let f = next.node_field(id)?;
        let raw = next.proto.source.get(f.span.clone()).ok_or("Invalid node span")?.to_string();
        if let Some(before) = before {
            let n = next.nodes.iter().find(|n| n.id == before).ok_or("Missing sibling")?;
            if n.id == id || n.parent != parent {
                return Err("Invalid sibling destination".into());
            }
        }
        let mut source = next.proto.source.clone();
        source.replace_range(f.span.clone(), "");
        let without = TextProto::parse(source)?;
        let pos = if let Some(before) = before {
            without.fields.iter().find(|f| f.name == "nodes" && without.text(&f.children, "id") == before).ok_or("Missing sibling")?.span.start
        } else {
            without.source.len()
        };
        let mut source = without.source;
        source.insert_str(pos, &format!("{raw}\n"));
        Self::parse(source)
    }
}
fn valid_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 256 || id.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        Err("Node ID must be nonempty, <=256 bytes, without slashes or control characters".into())
    } else {
        Ok(())
    }
}
