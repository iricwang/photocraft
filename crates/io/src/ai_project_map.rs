//! PhotoCraft project memory in the PSD plug-in resource range (Adobe PSD specification).
//! The resource is identified by both its ID and magic; unrelated plug-in data is preserved.
use photocraft_doc::ai::{MAX_PROJECT_REFERENCES, MAX_REFERENCE_BYTES, ProjectEvent, ProjectMemory, ProjectReference};
use photocraft_psd::resources::ImageResource;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(crate) const RESOURCE_ID: u16 = 4999;
const MAGIC: &[u8] = b"PhotoCraft.AI.Project\0\x01";
const MAX_HEADER: usize = 1 << 20;

#[derive(Serialize, Deserialize)]
struct Header {
    id: String,
    next_sequence: u64,
    last_prompt: String,
    events: Vec<ProjectEvent>,
    references: Vec<Reference>,
}
#[derive(Serialize, Deserialize)]
struct Reference {
    id: u64,
    name: String,
    bytes: usize,
}

pub(crate) fn matches(id: u16, bytes: &[u8]) -> bool {
    id == RESOURCE_ID && bytes.starts_with(MAGIC)
}

fn encode(memory: &ProjectMemory) -> Result<Vec<u8>, String> {
    if !memory.is_valid() {
        return Err("invalid AI project memory".into());
    }
    let header = Header {
        id: memory.id.clone(),
        next_sequence: memory.next_sequence,
        last_prompt: memory.last_prompt.clone(),
        events: memory.events.clone(),
        references: memory.references.iter().map(|r| Reference { id: r.id, name: r.name.clone(), bytes: r.png.len() }).collect(),
    };
    let header = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
    if header.len() > MAX_HEADER {
        return Err("AI project header is too large".into());
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(header.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&header);
    for reference in &memory.references {
        bytes.extend_from_slice(&reference.png);
    }
    Ok(bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<ProjectMemory, String> {
    if bytes.len() > MAX_HEADER + MAX_PROJECT_REFERENCES * MAX_REFERENCE_BYTES + 128 || !bytes.starts_with(MAGIC) {
        return Err("invalid or oversized AI project resource".into());
    }
    let raw_len: [u8; 4] = bytes.get(MAGIC.len()..MAGIC.len() + 4).ok_or("truncated AI project header")?.try_into().map_err(|_| "invalid header length")?;
    let len = u32::from_be_bytes(raw_len) as usize;
    if len > MAX_HEADER {
        return Err("AI project header exceeds limit".into());
    }
    let start = MAGIC.len() + 4;
    let end = start.checked_add(len).ok_or("AI project header overflow")?;
    let header: Header = serde_json::from_slice(bytes.get(start..end).ok_or("truncated AI project header")?).map_err(|e| e.to_string())?;
    if header.references.len() > MAX_PROJECT_REFERENCES || header.events.len() > photocraft_doc::ai::MAX_PROJECT_EVENTS {
        return Err("AI project memory exceeds limit".into());
    }
    let mut memory =
        ProjectMemory { id: header.id, references: Vec::new(), events: header.events, next_sequence: header.next_sequence, last_prompt: header.last_prompt };
    let mut cursor = end;
    for r in header.references {
        if r.bytes > MAX_REFERENCE_BYTES {
            return Err("AI reference exceeds 8 MiB".into());
        }
        let next = cursor.checked_add(r.bytes).ok_or("AI reference length overflow")?;
        let png = bytes.get(cursor..next).ok_or("truncated AI reference")?;
        let limits = photocraft_codecs::Limits { max_width: 1536, max_height: 1536, max_pixels: 1536 * 1536, max_alloc: 64 << 20 };
        photocraft_codecs::decode_with(png, &photocraft_codecs::DecodeOptions { limits, ..Default::default() })
            .map_err(|e| format!("invalid AI reference: {e}"))?;
        memory.references.push(ProjectReference { id: r.id, name: r.name, png: Arc::new(png.to_vec()) });
        cursor = next;
    }
    if cursor != bytes.len() || !memory.is_valid() {
        return Err("invalid AI project contents".into());
    }
    Ok(memory)
}

pub(crate) fn append(doc: &photocraft_doc::Document, resources: &mut Vec<ImageResource>, warnings: &mut Vec<String>) {
    if doc.ai_project == ProjectMemory::default() {
        return;
    }
    match encode(&doc.ai_project) {
        Ok(data) => {
            let mut resource = ImageResource::new(RESOURCE_ID, data);
            resource.name = b"PhotoCraft AI Project".to_vec();
            resources.push(resource);
        }
        Err(error) => warnings.push(format!("AI project memory was not saved: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> ProjectMemory {
        let image = photocraft_codecs::Image::from_u8(2, 1, photocraft_codecs::ChannelLayout::Rgba, vec![100; 8]).unwrap();
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        let mut memory = ProjectMemory { id: "abcdef0123456789abcdef0123456789".into(), last_prompt: "Make it 3D".into(), ..Default::default() };
        memory.references.push(ProjectReference { id: 1, name: "style.png".into(), png: Arc::new(png) });
        memory.record("user", "Make it 3D");
        memory
    }
    #[test]
    fn project_references_round_trip_native_psd_psb_and_non_rgb() {
        for mode in [photocraft_color::ColorMode::Rgb, photocraft_color::ColorMode::Grayscale, photocraft_color::ColorMode::Multichannel] {
            let mut doc = photocraft_doc::Document::new("test", photocraft_geom::Size::new(2, 2), mode, photocraft_color::SampleType::U8);
            doc.ai_project = project();
            for extension in ["pcraft", "psd", "psb"] {
                let saved = crate::export(&doc, extension, &Default::default()).unwrap();
                let loaded = crate::import(extension, &saved.bytes).unwrap();
                assert_eq!(loaded.document.ai_project, doc.ai_project, "{mode:?} {extension}");
            }
        }
    }
    #[test]
    fn corrupt_resource_is_bounded_and_other_plugin_data_is_not_claimed() {
        let bytes = encode(&project()).unwrap();
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err());
        }
        assert!(!matches(RESOURCE_ID, b"unrelated plugin data"));
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&huge).is_err());
    }
}
