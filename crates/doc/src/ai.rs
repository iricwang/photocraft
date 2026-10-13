//! Native-project AI provenance. Never exported as image metadata or provider credentials.
use crate::{LayerId, Rect, Size, Surface};
use std::sync::Arc;

pub const MAX_PROJECT_REFERENCES: usize = 4;
pub const MAX_PROJECT_EVENTS: usize = 256;

/// Portable project context. Host paths, service credentials and Codex thread identities
/// deliberately do not belong to a document received from another person.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectMemory {
    pub id: String,
    pub references: Vec<ProjectReference>,
    pub events: Vec<ProjectEvent>,
    pub next_sequence: u64,
    pub last_prompt: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectReference {
    pub id: u64,
    pub name: String,
    pub png: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectEvent {
    pub sequence: u64,
    pub kind: String,
    pub summary: String,
}

/// Byte-bounded text without splitting UTF-8.
pub fn bounded_text(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

impl ProjectMemory {
    pub fn record(&mut self, kind: &str, summary: &str) {
        let sequence = self.next_sequence.max(1);
        let Some(next) = sequence.checked_add(1) else { return };
        self.next_sequence = next;
        self.events.push(ProjectEvent { sequence, kind: bounded_text(kind, 64), summary: bounded_text(summary, 2048) });
        let extra = self.events.len().saturating_sub(MAX_PROJECT_EVENTS);
        self.events.drain(..extra);
    }

    /// Audit and conversation memory survive Undo; references follow document history.
    pub fn retain_context_from(&mut self, current: &Self) {
        self.id.clone_from(&current.id);
        self.events.clone_from(&current.events);
        self.next_sequence = current.next_sequence;
        self.last_prompt.clone_from(&current.last_prompt);
    }

    pub fn is_valid(&self) -> bool {
        (self.id.is_empty() || (self.id.len() == 32 && self.id.bytes().all(|b| b.is_ascii_hexdigit())))
            && self.last_prompt.len() <= 4096
            && self.references.len() <= MAX_PROJECT_REFERENCES
            && self.references.iter().all(|r| {
                r.id > 0
                    && r.name.len() <= 480
                    && !r.name.chars().any(char::is_control)
                    && r.png.len() <= MAX_REFERENCE_BYTES
                    && r.png.starts_with(b"\x89PNG\r\n\x1a\n")
            })
            && self.references.iter().enumerate().all(|(i, r)| self.references.iter().take(i).all(|other| other.id != r.id))
            && self.events.len() <= MAX_PROJECT_EVENTS
            && self.events.iter().all(|e| e.sequence > 0 && e.sequence < self.next_sequence && e.kind.len() <= 64 && e.summary.len() <= 2048)
            && self.events.windows(2).all(|pair| pair.first().zip(pair.get(1)).is_some_and(|(a, b)| a.sequence < b.sequence))
    }
}

pub const MAX_GROUPS: usize = 32;
pub const MAX_VERSIONS: usize = 32;
pub const MAX_REFERENCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationMode {
    #[default]
    Repaint,
    Component,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenerationGroup {
    pub id: u64,
    pub canvas: Size,
    pub area: Rect,
    pub frame: Option<Rect>,
    pub selection: Option<Surface>,
    pub reference_png: Option<Arc<Vec<u8>>>,
    pub mode: GenerationMode,
    pub context_png: Option<Arc<Vec<u8>>>,
    pub versions: Vec<GenerationVersion>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GenerationVersion {
    /// None after the result layer has been deleted and ids have been remapped on import.
    pub layer: Option<LayerId>,
    pub prompt: String,
    pub optimized_prompt: String,
    pub provider: String,
    pub model: String,
}

impl GenerationGroup {
    pub fn is_valid(&self) -> bool {
        self.id > 0
            && self.canvas.width > 0
            && self.canvas.height > 0
            && self.canvas.width <= 300_000
            && self.canvas.height <= 300_000
            && !self.area.is_empty()
            && self.area.intersect(&Rect::from_size(self.canvas)) == self.area
            && u64::from(self.area.width()) * u64::from(self.area.height()) <= 16 * 1024 * 1024
            && self.frame.is_none_or(|f| f.width() > 0 && f.width() == f.height() && f.width() <= 4096 && f.intersect(&self.area) == self.area)
            && self.reference_png.as_ref().is_none_or(|b| b.len() <= MAX_REFERENCE_BYTES)
            && self.context_png.as_ref().is_none_or(|b| b.len() <= MAX_REFERENCE_BYTES)
            && (self.mode != GenerationMode::Component || (self.frame.is_none() && self.reference_png.is_some() && self.context_png.is_some()))
            && (self.frame.is_none() || self.reference_png.is_some())
            && self.versions.len() <= MAX_VERSIONS
            && self.versions.iter().all(|v| v.prompt.len() <= 4096 && v.optimized_prompt.len() <= 16384 && v.model.len() <= 256 && v.provider.len() <= 32)
    }
}
