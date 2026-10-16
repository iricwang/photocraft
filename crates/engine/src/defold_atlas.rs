//! Pure managed-atlas encoding. Runtime names are independent of PNG filenames.
use crate::defold_project_model::{ProjectError, ProjectErrorCode, ProjectResult, ResourcePath};
use photocraft_defold_format::TextProto;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtlasFrame {
    pub id: String,
    pub image: ResourcePath,
}

fn invalid(message: impl Into<String>) -> ProjectError {
    ProjectError::new(ProjectErrorCode::UnsupportedGuiFeature, message)
}
fn validate_frame(frame: &AtlasFrame) -> ProjectResult<()> {
    if frame.id.is_empty() || frame.id.len() > 512 || frame.id.contains(['/', '\\']) || frame.id.chars().any(char::is_control) {
        return Err(invalid("Invalid atlas frame identity"));
    }
    Ok(())
}

/// A managed atlas contains explicit single-frame animations. This does not rewrite arbitrary
/// external atlases, whose packing settings, multi-frame animations and unknown fields are owned externally.
pub fn encode(frames: &[AtlasFrame]) -> ProjectResult<Vec<u8>> {
    let mut ids = BTreeSet::new();
    let mut ordered = frames.to_vec();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    let mut out = String::new();
    for frame in ordered {
        validate_frame(&frame)?;
        if !ids.insert(frame.id.clone()) {
            return Err(invalid("Duplicate atlas frame identity"));
        }
        let id = serde_json::to_string(&frame.id).map_err(|e| invalid(e.to_string()))?;
        let image = serde_json::to_string(frame.image.as_str()).map_err(|e| invalid(e.to_string()))?;
        out.push_str(&format!("animations {{\n  id: {id}\n  images {{ image: {image} }}\n  playback: PLAYBACK_ONCE_FORWARD\n  fps: 0\n}}\n"));
    }
    out.push_str("extrude_borders: 2\ninner_padding: 2\n");
    Ok(out.into_bytes())
}

/// Import both legacy basename frames and explicit single-frame animations for GUI preview.
/// Multi-frame animations are rejected, rather than silently importing the wrong frame.
pub fn decode(source: &str) -> ProjectResult<Vec<AtlasFrame>> {
    let proto = TextProto::parse(source.into()).map_err(invalid)?;
    if proto.fields.iter().any(|f| f.name == "rename_patterns" && !proto.text(&proto.fields, "rename_patterns").is_empty()) {
        return Err(invalid("Atlas rename patterns require the Defold animation resolver"));
    }
    let mut frames = Vec::new();
    let mut ids = BTreeSet::new();
    for field in &proto.fields {
        let (id, path) = match field.name.as_str() {
            "images" => {
                let path = proto.text(&field.children, "image");
                let stem = path
                    .rsplit('/')
                    .next()
                    .and_then(|name| name.rsplit_once('.').map(|(stem, _)| stem))
                    .ok_or_else(|| invalid("Atlas image has no filename extension"))?;
                (stem.to_string(), path)
            }
            "animations" => {
                let images: Vec<_> = field.children.iter().filter(|f| f.name == "images").collect();
                if images.len() != 1 {
                    return Err(invalid("Only single-frame animations can be imported as a GUI image"));
                }
                let image = images.first().ok_or_else(|| invalid("Animation has no frame"))?;
                (proto.text(&field.children, "id"), proto.text(&image.children, "image"))
            }
            _ => continue,
        };
        let frame = AtlasFrame { id, image: ResourcePath::parse(&path)? };
        validate_frame(&frame)?;
        if !ids.insert(frame.id.clone()) {
            return Err(invalid("Duplicate atlas frame identity"));
        }
        frames.push(frame);
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    #[test]
    fn explicit_frame_id_survives_readable_png_rename() {
        let frames = vec![AtlasFrame { id: "button_idle".into(), image: ResourcePath::parse("/photocraft_generated/menu/images/开始游戏.png").unwrap() }];
        let bytes = encode(&frames).unwrap();
        assert_eq!(decode(std::str::from_utf8(&bytes).unwrap()).unwrap(), frames);
        assert!(std::str::from_utf8(&bytes).unwrap().contains("id: \"button_idle\""));
    }
    #[test]
    fn import_legacy_and_explicit_frames() {
        let frames = decode("images { image: \"/ui/old.png\" } animations { id: \"public_name\" images { image: \"/ui/new.png\" } }").unwrap();
        assert_eq!(frames.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["old", "public_name"]);
    }
    #[test]
    fn multi_frame_and_duplicate_ids_are_not_silently_flattened() {
        assert!(decode("animations { id: \"run\" images {image: \"/a.png\"} images {image: \"/b.png\"} }").is_err());
        assert!(decode("images {image: \"/a.png\"} animations {id: \"a\" images {image: \"/b.png\"} }").is_err());
    }
}
