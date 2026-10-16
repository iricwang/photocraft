//! Native AI generation transactions. Results can become persistent, undoable layers or
//! read-only candidates that require a checked Apply command.

use base64::Engine as _;
use std::sync::Arc;

use photocraft_doc::ai::GenerationMode;
use photocraft_doc::{Document, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::{CommandSpec, EngineError, Result, Session};

const MAX_TEST_PIXELS: u64 = 1024 * 1024;
const MAX_TARGET_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_REFERENCE_EDGE: u32 = 1024;
const MAX_ID: u64 = (1 << 30) - 1;

#[derive(Default)]
pub struct AiState {
    candidate: Option<Candidate>,
    pending: Option<Candidate>,
    next_id: u64,
    last_applied: Option<Value>,
    last_layer: Option<Value>,
}

#[derive(Clone)]
pub struct Candidate {
    pub id: u64,
    pub base_revision: u64,
    pub layer: LayerId,
    pub area: Rect,
    // Selected edits return the context frame, not an image to stretch into the selection.
    frame: Option<Rect>,
    selection: Option<Surface>,
    source: Option<Arc<Vec<u8>>>,
    mode: GenerationMode,
    context: Option<Arc<Vec<u8>>>,
    group_id: Option<u64>,
    model: String,
    optimized_prompt: String,
    pub prompt: String,
    pub provider: String,
    new_layer: bool,
    source_closed: bool,
    base: Arc<Document>,
    preview: Arc<Document>,
    pixels: Surface,
}

impl Candidate {
    pub fn is_current(&self, s: &Session) -> bool {
        s.active().is_some_and(|st| st.revision == self.base_revision && Arc::ptr_eq(&st.doc, &self.base) && st.active_layer == Some(self.layer))
    }

    /// Read-only preview; stale candidates are never displayed over a newer document.
    pub fn preview(&self, s: &Session) -> Option<Arc<Document>> {
        self.is_current(s).then(|| self.preview.clone())
    }

    fn metadata(&self, s: &Session) -> Value {
        json!({"candidateId": self.id, "documentId": self.base.id.0, "baseRevision": self.base_revision,
            "canvas":[self.base.size.width,self.base.size.height],"hasSelection":self.selection.is_some(),
            "layerId": self.layer.0, "area": [self.area.x0, self.area.y0, self.area.x1, self.area.y1], "areaFormat":"[x0,y0,x1,y1] in original document pixels",
            "prompt": self.prompt, "provider": self.provider, "mode":self.mode, "destination": if self.new_layer { "layer" } else { "candidate" }, "stale": !self.is_current(s)})
    }
}

impl AiState {
    pub fn candidate(&self) -> Option<&Candidate> {
        self.candidate.as_ref()
    }

    pub(crate) fn forget_document(&mut self, id: photocraft_doc::DocId) {
        if self.pending.as_ref().is_some_and(|c| c.base.id == id) {
            if let Some(pending) = self.pending.as_mut().filter(|c| c.new_layer) {
                pending.source_closed = true;
            } else {
                self.pending = None;
            }
        }
        if self.candidate.as_ref().is_some_and(|c| c.base.id == id) {
            self.candidate = None;
        }
    }
}

fn bad(cmd: &str, msg: &str) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn target(s: &Session) -> std::result::Result<(), String> {
    target_with_limit(s, MAX_TARGET_PIXELS, true)
}

fn test_target(s: &Session) -> std::result::Result<(), String> {
    target_with_limit(s, MAX_TEST_PIXELS, false)
}

fn target_with_limit(s: &Session, limit: u64, linked_allowed: bool) -> std::result::Result<(), String> {
    if crate::channel_cmds::edits_channel(s) || s.tools.mask_targeted {
        return Err("AI candidates require layer pixels, not a mask or channel".into());
    }
    let st = s.active().ok_or("no document open")?;
    if !matches!(st.doc.mode, photocraft_doc::ColorMode::Rgb | photocraft_doc::ColorMode::Grayscale | photocraft_doc::ColorMode::Cmyk) {
        return Err("AI generation requires RGB, Grayscale or CMYK; convert the document first".into());
    }
    if st.channel_view.target != crate::channel_cmds::ChannelTarget::Composite || crate::mask_view_cmds::current(st).is_some() {
        return Err("AI candidates require the composite channel and layer pixels".into());
    }
    if st.floating.is_some() {
        return Err("drop the floating selection before creating a candidate".into());
    }
    let layer = crate::active_layer_of(s)?;
    let node_art = linked_allowed
        .then(|| crate::defold_design::node_for_layer(&st.doc, layer.id))
        .flatten()
        .and_then(|n| st.doc.layer(photocraft_doc::LayerId(n.artwork)))
        .filter(|l| l.surface().is_some());
    if !matches!(layer.content, LayerContent::Raster(_)) && node_art.is_none() {
        return Err("AI candidates require a raster layer".into());
    }
    crate::commands::check_pixels_unlocked(&st.doc, layer.id).map_err(|e| e.to_string())?;
    if let Some(art) = node_art {
        crate::commands::check_pixels_unlocked(&st.doc, art.id).map_err(|e| e.to_string())?;
    }
    let bounds = node_art
        .and_then(|l| photocraft_compose::composite_bounds(l, st.doc.bounds()))
        .map(|r| r.intersect(&st.doc.bounds()))
        .filter(|r| !r.is_empty())
        .unwrap_or(st.doc.bounds());
    let area = st.doc.selection.as_ref().map_or(bounds, |s| s.content_bounds().intersect(&st.doc.bounds()));
    let pixels = u64::from(area.width()) * u64::from(area.height());
    if area.is_empty() || pixels == 0 {
        return Err("the selection is empty".into());
    }
    if pixels > limit {
        return Err(format!("AI generation supports an area up to {limit} pixels; select a smaller area"));
    }
    Ok(())
}

fn prepare(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.prepareTestCandidate";
    let object = p.as_object().ok_or_else(|| bad(ID, "params must be an object"))?;
    if object.keys().any(|k| k != "prompt") {
        return Err(bad(ID, "only prompt is supported by the local test provider"));
    }
    let prompt = match p.get("prompt") {
        None => "".to_owned(),
        Some(Value::String(text)) if text.len() <= 4096 => text.trim().to_owned(),
        Some(_) => return Err(bad(ID, "prompt must be a string of at most 4096 bytes")),
    };
    let color = s.tools.foreground;
    if !color.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
        return Err(bad(ID, "foreground color must have four finite components in 0..1"));
    }
    let id =
        s.ai.next_id.checked_add(1).filter(|id| *id <= MAX_ID).ok_or_else(|| EngineError::Other("AI candidate limit reached; restart the session".into()))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let layer = st.active_layer.ok_or_else(|| EngineError::Other("no active layer".into()))?;
    let selection = st.doc.selection.as_ref().ok_or_else(|| EngineError::Other("select an area first".into()))?;
    let area = selection.content_bounds().intersect(&st.doc.bounds());
    let mut pixels = st.doc.layer(layer).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(layer))?.clone();
    let locks = st.doc.effective_locks(layer);
    // Existing native fill handles feathered coverage, COW tiles, color modes and every depth.
    // In particular it never re-encodes pixels at zero coverage.
    crate::pixels::try_fill_surface(&mut pixels, area, color, Some(selection), locks.transparency)?;
    let mut preview = (*st.doc).clone();
    *preview.layer_mut(layer).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(layer))? = pixels.clone();
    let candidate = Candidate {
        id,
        base_revision: st.revision,
        layer,
        area,
        frame: None,
        selection: st.doc.selection.clone(),
        source: None,
        mode: GenerationMode::Repaint,
        context: None,
        group_id: None,
        model: String::new(),
        optimized_prompt: String::new(),
        prompt,
        provider: "local-test".into(),
        new_layer: false,
        source_closed: false,
        base: st.doc.clone(),
        preview: Arc::new(preview),
        pixels,
    };
    let result = candidate.metadata(s);
    s.ai.candidate = Some(candidate);
    s.ai.next_id = id;
    Ok(result)
}

fn candidate_id(cmd: &str, p: &Value) -> Result<u64> {
    let object = p.as_object().ok_or_else(|| bad(cmd, "params must be an object"))?;
    if object.len() != 1 {
        return Err(bad(cmd, "exactly candidateId is required"));
    }
    p.get("candidateId")
        .and_then(Value::as_u64)
        .filter(|id| (1..=MAX_ID).contains(id))
        .ok_or_else(|| bad(cmd, "candidateId must be a positive candidate identifier"))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let id = candidate_id("ai.applyCandidate", p)?;
    if let Some(receipt) = &s.ai.last_applied
        && receipt["candidateId"].as_u64() == Some(id)
    {
        return Ok(receipt.clone());
    }
    let candidate = s.ai.candidate.as_ref().ok_or_else(|| EngineError::Other("no AI candidate".into()))?;
    if candidate.id != id {
        return Err(bad("ai.applyCandidate", "candidateId does not match the pending candidate"));
    }
    if !candidate.is_current(s) {
        return Err(EngineError::Other("the document changed; create a new candidate".into()));
    }
    target_with_limit(s, if candidate.provider == "local-test" { MAX_TEST_PIXELS } else { MAX_TARGET_PIXELS }, false).map_err(EngineError::Other)?;
    let (layer, pixels, area) = (candidate.layer, candidate.pixels.clone(), candidate.area);
    s.edit("Apply AI Candidate", |doc, _| {
        *doc.layer_mut(layer).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(layer))? = pixels;
        Ok(())
    })?;
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    st.last_damage = Some(area);
    let receipt = json!({"candidateId": id, "applied": true, "revision": st.revision});
    s.ai.candidate = None;
    s.ai.last_applied = Some(receipt.clone());
    Ok(receipt)
}

fn discard(s: &mut Session, p: &Value) -> Result<Value> {
    let id = candidate_id("ai.discardCandidate", p)?;
    if s.ai.candidate.as_ref().is_none_or(|c| c.id != id) {
        return Err(bad("ai.discardCandidate", "candidateId does not match the pending candidate"));
    }
    s.ai.candidate = None;
    Ok(json!({"candidateId": id, "discarded": true}))
}

fn inspect(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(
        json!({"candidate": s.ai.candidate.as_ref().map(|c| c.metadata(s)), "pending": s.ai.pending.as_ref().map(|c| c.metadata(s)), "lastApplied": s.ai.last_applied,"lastLayer":s.ai.last_layer}),
    )
}

fn begin(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.beginGeneration";
    let obj = p.as_object().ok_or_else(|| bad(ID, "params must be an object"))?;
    if obj.keys().any(|k| !["prompt", "provider", "reference", "destination", "model", "mode"].contains(&k.as_str())) {
        return Err(bad(ID, "unknown generation parameter"));
    }
    let prompt = p
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty() && v.len() <= 4096)
        .ok_or_else(|| bad(ID, "prompt must contain 1..4096 bytes"))?
        .trim()
        .to_owned();
    let provider = p
        .get("provider")
        .and_then(Value::as_str)
        .filter(|v| ["images-api", "codex"].contains(v))
        .ok_or_else(|| bad(ID, "provider must be images-api or codex"))?
        .to_owned();
    let reference = p.get("reference").map_or(Some(false), Value::as_bool).ok_or_else(|| bad(ID, "reference must be boolean"))?;
    let mode: GenerationMode =
        p.get("mode").map(|v| serde_json::from_value(v.clone())).transpose().map_err(|_| bad(ID, "mode must be repaint or component"))?.unwrap_or_default();
    let destination = p
        .get("destination")
        .map_or(Some("candidate"), Value::as_str)
        .filter(|v| ["candidate", "layer"].contains(v))
        .ok_or_else(|| bad(ID, "destination must be candidate or layer"))?;
    if s.ai.pending.is_some() {
        return Err(bad(ID, "a generation is already pending"));
    }
    let id = s.ai.next_id.checked_add(1).filter(|i| *i <= MAX_ID).ok_or_else(|| bad(ID, "generation limit reached"))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if mode == GenerationMode::Component && (destination != "layer" || st.doc.selection.is_none()) {
        return Err(bad(ID, "select the component area first; transparent components require a new layer"));
    }
    let needs_canvas = provider == "codex" || mode == GenerationMode::Component;
    if needs_canvas && u64::from(st.doc.size.width) * u64::from(st.doc.size.height) > 64 * 1024 * 1024 {
        return Err(bad(ID, "generation canvas references support canvases up to 64 megapixels"));
    }
    if destination == "layer" && st.doc.ai_generations.len() >= photocraft_doc::ai::MAX_GROUPS {
        return Err(bad(ID, "AI history is full; remove a generation record first"));
    }
    let layer = st.active_layer.ok_or_else(|| bad(ID, "no active layer"))?;
    if destination == "candidate" && st.doc.layer(layer).is_some_and(|l| !matches!(l.content, LayerContent::Raster(_))) {
        return Err(bad(ID, "Generate to a new layer for a GUI group or text node"));
    }
    let node_area = crate::defold_design::node_for_layer(&st.doc, layer)
        .and_then(|n| st.doc.layer(photocraft_doc::LayerId(n.artwork)))
        .and_then(|l| photocraft_compose::composite_bounds(l, st.doc.bounds()))
        .map(|r| r.intersect(&st.doc.bounds()))
        .filter(|r| !r.is_empty());
    let area = st.doc.selection.as_ref().map_or(node_area.unwrap_or(st.doc.bounds()), |m| m.content_bounds().intersect(&st.doc.bounds()));
    let pixels = st
        .doc
        .layer(layer)
        .and_then(|l| l.surface())
        .or_else(|| {
            crate::defold_design::node_for_layer(&st.doc, layer).and_then(|n| st.doc.layer(photocraft_doc::LayerId(n.artwork))).and_then(|l| l.surface())
        })
        .ok_or(EngineError::NoLayer(layer))?
        .clone();
    let frame = if mode == GenerationMode::Repaint {
        (st.doc.selection.is_some() || node_area.is_some()).then(|| edit_frame(area, st.doc.bounds())).transpose()?
    } else {
        None
    };
    // Keep a local full-canvas snapshot for on-demand MCP reads, separate from inline inputs.
    let context = if needs_canvas {
        Some(crate::ai_component::reference(&st.doc, st.doc.bounds(), if mode == GenerationMode::Component { 1024 } else { 1536 })?)
    } else {
        None
    };
    // Local edits always see the visible composite, even if the active layer is empty.
    let source = if mode == GenerationMode::Component {
        Some(crate::ai_component::reference(&st.doc, area, 2048)?)
    } else if let Some(frame) = frame {
        Some(edit_reference(&st.doc, frame)?)
    } else if reference {
        let mut crop = Document::new("AI reference", photocraft_geom::Size::new(area.width(), area.height()), st.doc.mode, st.doc.depth);
        crop.icc_profile = st.doc.icc_profile.clone();
        let origin = Rect::new(0, 0, area.width() as i32, area.height() as i32);
        let mut surface = Surface::new(pixels.format());
        surface.write_region(origin, &pixels.read_region(area));
        // Bound provider input independently of the full-resolution destination.
        let scale = (f64::from(MAX_REFERENCE_EDGE) / f64::from(area.width().max(area.height()))).min(1.0);
        if scale < 1.0 {
            let width = (f64::from(area.width()) * scale).floor().max(1.0) as u32;
            let height = (f64::from(area.height()) * scale).floor().max(1.0) as u32;
            surface = photocraft_algo::resample::resize_surface_in_canvas(
                &surface,
                f64::from(width) / f64::from(area.width()),
                f64::from(height) / f64::from(area.height()),
                photocraft_algo::resample::Resample::Bicubic,
                origin,
            );
            crop.size = photocraft_geom::Size::new(width, height);
        }
        crop.layers.push(photocraft_doc::Layer::new("Reference", LayerContent::Raster(surface)));
        crate::color_cmds::convert_document(&mut crop, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)?;
        let bytes = photocraft_io::export(&crop, "reference.png", &photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() })
            .map_err(|e| EngineError::Other(e.to_string()))?
            .bytes;
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    } else {
        None
    };
    let pending = Candidate {
        id,
        base_revision: st.revision,
        layer,
        area,
        frame,
        mode,
        context: context
            .as_ref()
            .map(|v| base64::engine::general_purpose::STANDARD.decode(v).map(Arc::new))
            .transpose()
            .map_err(|_| bad(ID, "invalid context"))?,
        selection: st.doc.selection.clone(),
        source: source
            .as_ref()
            .map(|v| base64::engine::general_purpose::STANDARD.decode(v).map(Arc::new))
            .transpose()
            .map_err(|_| bad(ID, "invalid reference"))?,
        group_id: None,
        model: text_param(p, "model", 256, ID)?,
        optimized_prompt: String::new(),
        prompt,
        provider,
        new_layer: destination == "layer",
        source_closed: false,
        base: st.doc.clone(),
        preview: st.doc.clone(),
        pixels,
    };
    let mut result = pending.metadata(s);
    result["sourcePng"] = json!(source);
    result["canvasPng"] = json!(context);
    result["contextPng"] = if mode == GenerationMode::Component { json!(context) } else { Value::Null };
    result["editInstructions"] = json!(instructions(&pending));
    s.ai.pending = Some(pending);
    s.ai.next_id = id;
    Ok(result)
}

fn text_param(p: &Value, key: &str, max: usize, cmd: &str) -> Result<String> {
    p.get(key).map_or(Some(""), Value::as_str).filter(|s| s.len() <= max).map(str::to_owned).ok_or_else(|| bad(cmd, &format!("invalid {key}")))
}

pub(crate) fn retry(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.retryGeneration";
    let obj = p.as_object().ok_or_else(|| bad(ID, "params must be an object"))?;
    if obj.keys().any(|k| !["groupId", "prompt", "provider", "model"].contains(&k.as_str())) || s.ai.pending.is_some() {
        return Err(bad(ID, "unknown parameter or another generation is pending"));
    }
    let group_id = p.get("groupId").and_then(Value::as_u64).ok_or_else(|| bad(ID, "groupId is required"))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let group = st.doc.ai_generations.iter().find(|g| g.id == group_id).ok_or_else(|| bad(ID, "generation group not found"))?;
    if group.canvas != st.doc.size || st.floating.is_some() || group.versions.len() >= photocraft_doc::ai::MAX_VERSIONS {
        return Err(bad(ID, "canvas changed, floating selection active, or version limit reached"));
    }
    if !matches!(st.doc.mode, photocraft_doc::ColorMode::Rgb | photocraft_doc::ColorMode::Grayscale | photocraft_doc::ColorMode::Cmyk) {
        return Err(bad(ID, "convert the document to RGB, Grayscale or CMYK first"));
    }
    crate::ai_history_cmds::validate_group(group)?;
    let prompt = text_param(p, "prompt", 4096, ID)?;
    let provider = text_param(p, "provider", 32, ID)?;
    if prompt.trim().is_empty() || !["codex", "images-api"].contains(&provider.as_str()) {
        return Err(bad(ID, "prompt and supported provider are required"));
    }
    let id = s.ai.next_id.checked_add(1).filter(|i| *i <= MAX_ID).ok_or_else(|| bad(ID, "generation limit reached"))?;
    let pending = Candidate {
        id,
        base_revision: st.revision,
        layer: st.active_layer.unwrap_or(LayerId(0)),
        area: group.area,
        frame: group.frame,
        selection: group.selection.clone(),
        source: group.reference_png.clone(),
        mode: group.mode,
        context: group.context_png.clone(),
        group_id: Some(group.id),
        model: text_param(p, "model", 256, ID)?,
        optimized_prompt: String::new(),
        prompt,
        provider,
        new_layer: true,
        source_closed: false,
        base: st.doc.clone(),
        preview: st.doc.clone(),
        pixels: Surface::new(st.doc.pixel_format()),
    };
    let mut result = pending.metadata(s);
    result["sourcePng"] = json!(pending.source.as_ref().map(|b| base64::engine::general_purpose::STANDARD.encode(b.as_slice())));
    result["canvasPng"] = json!(pending.context.as_ref().map(|b| base64::engine::general_purpose::STANDARD.encode(b.as_slice())));
    result["contextPng"] = if pending.mode == GenerationMode::Component { result["canvasPng"].clone() } else { Value::Null };
    result["editInstructions"] = json!(instructions(&pending));
    s.ai.pending = Some(pending);
    s.ai.next_id = id;
    Ok(result)
}

fn instructions(c: &Candidate) -> Option<String> {
    if c.mode == GenerationMode::Component {
        Some(crate::ai_component::instructions(c.area, c.base.size))
    } else {
        c.frame.map(|f| edit_instructions(c.area, f))
    }
}

fn edit_frame(area: Rect, bounds: Rect) -> Result<Rect> {
    let edge = area.width().max(area.height());
    if edge > 4096 {
        return Err(bad("ai.beginGeneration", "local edit context supports a selection edge up to 4096 pixels"));
    }
    let side = (edge + edge / 8).min(4096) as i32;
    let origin = |lo: i32, hi: i32, end: i32| {
        if end >= side { (lo + (hi - lo - side) / 2).clamp(0, end - side) } else { (end - side) / 2 }
    };
    let x = origin(area.x0, area.x1, bounds.x1);
    let y = origin(area.y0, area.y1, bounds.y1);
    Ok(Rect::new(x, y, x + side, y + side))
}

fn edit_instructions(area: Rect, frame: Rect) -> String {
    let unit = 100.0 / f64::from(frame.width());
    format!(
        "The reference image is a square context frame. \
         Edit ONLY the rectangle x={:.3}%..{:.3}%, y={:.3}%..{:.3}% (origin top-left). \
         Return the entire square frame at 1024x1024 with the same framing, without cropping, resizing the frame content, or adding borders. \
         Preserve content and padding outside the rectangle. Inside it, follow the user's requested change.",
        f64::from(area.x0 - frame.x0) * unit,
        f64::from(area.x1 - frame.x0) * unit,
        f64::from(area.y0 - frame.y0) * unit,
        f64::from(area.y1 - frame.y0) * unit,
    )
}

fn edit_reference(doc: &Document, frame: Rect) -> Result<String> {
    let mut rgb = doc.clone();
    crate::color_cmds::convert_document(&mut rgb, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let mut surface = Surface::new(photocraft_color::PixelFormat::RGBA8.with_sample(photocraft_color::SampleType::F32));
    // Render bounded strips; neither a huge canvas nor a deep layer stack allocates a full-canvas buffer.
    let visible = frame.intersect(&doc.bounds());
    for y in (visible.y0..visible.y1).step_by(256) {
        let strip = Rect::new(visible.x0, y, visible.x1, (y + 256).min(visible.y1));
        let rendered = photocraft_compose::render(&rgb, strip);
        for (i, pixel) in rendered.px.iter().enumerate() {
            let x = strip.x0 + (i % strip.width() as usize) as i32;
            let y = strip.y0 + (i / strip.width() as usize) as i32;
            surface.write_pixel(x - frame.x0, y - frame.y0, pixel);
        }
    }
    let origin = Rect::new(0, 0, frame.width() as i32, frame.height() as i32);
    let reference_edge = frame.width().min(MAX_REFERENCE_EDGE);
    let scale = f64::from(reference_edge) / f64::from(frame.width());
    let surface = photocraft_algo::resample::resize_surface_in_canvas(&surface, scale, scale, photocraft_algo::resample::Resample::Bicubic, origin);
    let mut reference = Document::new(
        "AI edit context",
        photocraft_geom::Size::new(reference_edge, reference_edge),
        photocraft_doc::ColorMode::Rgb,
        photocraft_color::SampleType::F32,
    );
    reference.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    reference.layers.push(photocraft_doc::Layer::new("Reference", LayerContent::Raster(surface)));
    let bytes = photocraft_io::export(&reference, "reference.png", &photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() })
        .map_err(|e| EngineError::Other(e.to_string()))?
        .bytes;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn mapped_result(c: &Candidate, image: &Document) -> Result<(Surface, Rect)> {
    if c.mode == GenerationMode::Component {
        return crate::ai_component::mapped_result(image, c.area, c.area, c.base.pixel_format());
    }
    let surface = image.layers.first().and_then(|l| l.surface()).ok_or_else(|| bad("ai.receiveCandidate", "image contains no pixels"))?;
    let frame = c.frame.unwrap_or(c.area);
    if c.frame.is_some() && image.size.width != image.size.height {
        return Err(bad(
            "ai.receiveCandidate",
            "local edit returned the wrong aspect ratio; expected the complete square context frame, not a cropped selection",
        ));
    }
    if image.size.width == 0 || image.size.height == 0 || frame.width() == 0 || frame.height() == 0 {
        return Err(bad("ai.receiveCandidate", "image and destination must have nonzero dimensions"));
    }
    let scale = (f64::from(frame.width()) / f64::from(image.size.width)).min(f64::from(frame.height()) / f64::from(image.size.height));
    // Full-image results fit inside the original canvas; never stretch or crop to fill it.
    // Selection edits retain their square context and original document-coordinate mapping.
    let area = if c.frame.is_some() {
        c.area
    } else {
        let width = (f64::from(image.size.width) * scale).round().clamp(1.0, f64::from(frame.width())) as u32;
        let height = (f64::from(image.size.height) * scale).round().clamp(1.0, f64::from(frame.height())) as u32;
        Rect::from_xywh(frame.x0 + ((frame.width() - width) / 2) as i32, frame.y0 + ((frame.height() - height) / 2) as i32, width, height)
    };
    let resized = photocraft_algo::resample::resize_surface_in_canvas(surface, scale, scale, photocraft_algo::resample::Resample::Bicubic, image.bounds())
        .convert(c.base.pixel_format());
    Ok((resized, area))
}

fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    let id = candidate_id("ai.cancelGeneration", p)?;
    if s.ai.pending.as_ref().is_some_and(|c| c.id == id) {
        s.ai.pending = None;
    }
    Ok(json!({"candidateId": id, "cancelled": true}))
}

fn receive(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.receiveCandidate";
    let obj = p.as_object().ok_or_else(|| bad(ID, "params must be an object"))?;
    if obj.keys().any(|k| !["candidateId", "imageBase64", "optimizedPrompt", "model", "expandComponent", "components"].contains(&k.as_str()))
        || !obj.contains_key("imageBase64")
    {
        return Err(bad(ID, "candidateId and imageBase64 are required"));
    }
    let id = candidate_id(ID, &json!({"candidateId": p.get("candidateId")}))?;
    let mut c = s.ai.pending.as_ref().filter(|c| c.id == id).ok_or_else(|| bad(ID, "generation is no longer pending"))?.clone();
    let expand = match p.get("expandComponent") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| bad(ID, "expandComponent must be boolean"))?,
    };
    if expand && c.mode != GenerationMode::Component {
        return Err(bad(ID, "expandComponent requires component mode"));
    }
    let components: Vec<crate::ai_assets::AssetSpec> = match p.get("components") {
        None => Vec::new(),
        Some(value) => {
            if value.as_array().is_none_or(|v| v.len() > 12) {
                return Err(bad(ID, "components must be an array of at most 12 items"));
            }
            serde_json::from_value(value.clone()).map_err(|_| bad(ID, "invalid component specification"))?
        }
    };
    if !components.is_empty() {
        crate::ai_assets::validate(&components).map_err(|e| bad(ID, &e))?;
        if c.mode != GenerationMode::Component || !c.new_layer {
            return Err(bad(ID, "component atlas requires component mode and layer destination"));
        }
    }
    c.optimized_prompt = text_param(p, "optimizedPrompt", 16384, ID)?;
    if p.get("model").is_some() {
        c.model = text_param(p, "model", 256, ID)?;
    }
    if !c.new_layer && !c.is_current(s) {
        return Err(bad(ID, "the document changed; generate again"));
    }
    if !c.new_layer {
        target(s).map_err(EngineError::Other)?;
    }
    let encoded = p.get("imageBase64").and_then(Value::as_str).filter(|v| v.len() <= 48 * 1024 * 1024).ok_or_else(|| bad(ID, "image exceeds 48 MiB"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| bad(ID, "invalid image base64"))?;
    let format = photocraft_codecs::detect(&bytes);
    if !matches!(format, Some(photocraft_codecs::Format::Png | photocraft_codecs::Format::Jpeg | photocraft_codecs::Format::WebP)) {
        return Err(bad(ID, "a PNG, JPEG or WebP image is required"));
    }
    let img = photocraft_codecs::decode_with(
        &bytes,
        &photocraft_codecs::DecodeOptions {
            limits: photocraft_codecs::Limits { max_width: 4096, max_height: 4096, max_pixels: 4 * 1024 * 1024, max_alloc: 64 << 20 },
            ..Default::default()
        },
    )
    .map_err(|e| bad(ID, &e.to_string()))?;
    // Re-encode the bounded decoded image, so document import cannot re-open an unbounded container.
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| bad(ID, &e.to_string()))?;
    let mut doc = photocraft_io::import("candidate.png", &png).map_err(|e| bad(ID, &e.to_string()))?.document;
    if doc.icc_profile.is_none() && doc.mode == photocraft_doc::ColorMode::Rgb {
        doc.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    }
    if !components.is_empty() {
        return receive_assets(s, &c, doc, &components);
    }
    if c.new_layer {
        return receive_layer(s, &c, doc, expand);
    }
    crate::color_cmds::convert_document(&mut doc, &crate::color_cmds::document_profile(&c.base), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let (resized, area) = mapped_result(&c, &doc)?;
    let frame = c.frame.unwrap_or(area);
    let fmt = c.pixels.format();
    let resized = resized.convert(fmt);
    let n = fmt.channels();
    let mut old = vec![0.0; n];
    let mut new = vec![0.0; n];
    let alpha_lock = c.base.effective_locks(c.layer).transparency;
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let k = c.selection.as_ref().map_or(1.0, |m| m.sample_channel(x, y, 0)).clamp(0.0, 1.0);
            if k == 0.0 {
                continue;
            }
            c.pixels.read_pixel(x, y, &mut old);
            resized.read_pixel(x - frame.x0, y - frame.y0, &mut new);
            let a = if fmt.alpha { old.last().copied().unwrap_or(1.0) } else { 1.0 };
            if alpha_lock && a == 0.0 {
                continue;
            }
            let b = if fmt.alpha { new.last().copied().unwrap_or(1.0) } else { 1.0 };
            let out_a = if alpha_lock { a } else { a * (1.0 - k) + b * k };
            for (i, (dst, src)) in old.iter_mut().zip(&new).enumerate() {
                *dst = if fmt.alpha && i == n - 1 {
                    out_a
                } else if alpha_lock {
                    *dst * (1.0 - k) + *src * k
                } else if out_a > 0.0 {
                    (*dst * a * (1.0 - k) + *src * b * k) / out_a
                } else {
                    0.0
                };
            }
            c.pixels.write_pixel(x, y, &old);
        }
    }
    let mut preview = (*c.base).clone();
    *preview.layer_mut(c.layer).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(c.layer))? = c.pixels.clone();
    c.preview = Arc::new(preview);
    let mut result = c.metadata(s);
    result["placement"] = json!({"fit":if c.mode == GenerationMode::Component{"component"}else if c.frame.is_some(){"selection-context"}else{"contain"},"hasMargins":c.mode != GenerationMode::Component && c.frame.is_none() && area != c.area,"sourceSize":[doc.size.width,doc.size.height],"bounds":[area.x0,area.y0,area.width(),area.height()]});
    s.ai.candidate = Some(c);
    s.ai.pending = None;
    Ok(result)
}

fn receive_assets(s: &mut Session, c: &Candidate, image: Document, specs: &[crate::ai_assets::AssetSpec]) -> Result<Value> {
    let assets = crate::ai_assets::split(&image, specs).map_err(|e| bad("ai.receiveCandidate", &e))?;
    let children: Vec<_> = assets.iter().map(crate::ai_assets::Asset::layer).collect();
    let component_ids: Vec<_> = children.iter().map(|layer| json!({"name":layer.name,"layerId":layer.id.0})).collect();
    let group = photocraft_doc::Layer::group("AI components", children);
    let id = group.id;
    // An asset board keeps original resolution and never covers or changes the source artwork.
    let mut doc = Document::new("AI components", image.size, image.mode, image.depth);
    doc.icc_profile = image.icc_profile;
    let index = s.add_document(doc, None);
    if !s.set_active(index) {
        return Err(bad("ai.receiveCandidate", "asset document is unavailable"));
    }
    let receipt = s.edit("Add AI Components", |doc, active| {
        doc.layers.push(group);
        *active = Some(id);
        Ok(json!({"candidateId":c.id,"layerId":id.0,"documentId":doc.id.0,"name":"AI components","savedAsLayer":true,"components":component_ids}))
    })?;
    s.ai.pending = None;
    s.ai.last_layer = Some(receipt.clone());
    Ok(receipt)
}

fn receive_layer(s: &mut Session, c: &Candidate, mut image: Document, expand: bool) -> Result<Value> {
    const ID: &str = "ai.receiveCandidate";
    // A new root layer cannot overwrite ongoing edits or inherit a source group's locks.
    // If the canvas identity/geometry/working format changed, retain the result in a new document.
    let destination = s.documents().iter().position(|st| {
        !c.source_closed
            && st.doc.id == c.base.id
            && st.doc.size == c.base.size
            && st.doc.mode == c.base.mode
            && st.doc.depth == c.base.depth
            && st.doc.icc_profile == c.base.icc_profile
            && (c.base.defold.is_none()
                || (st.doc.defold.is_some()
                    && st.doc.defold.as_ref().and_then(|d| d.project.as_ref()).map(|p| (&p.project_id, &p.scene_id, &p.package_id))
                        == c.base.defold.as_ref().and_then(|d| d.project.as_ref()).map(|p| (&p.project_id, &p.scene_id, &p.package_id))
                    && crate::defold_design::node_for_layer(&st.doc, c.layer).is_some()
                    && st.doc.layer(c.layer).zip(c.base.layer(c.layer)).is_some_and(|(current, original)| {
                        current.content == original.content && current.mask == original.mask && current.vector_mask == original.vector_mask
                    })))
            && st.floating.is_none()
            && s.job_on(st.doc.id).is_none()
    });
    crate::color_cmds::convert_document(&mut image, &crate::color_cmds::document_profile(&c.base), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let (resized, area) = if c.mode == GenerationMode::Component {
        crate::ai_component::mapped_result(&image, c.area, if expand { c.base.bounds() } else { c.area }, c.base.pixel_format())?
    } else {
        mapped_result(c, &image)?
    };
    let frame = c.frame.unwrap_or(area);
    let mut pixels = Surface::new(c.base.pixel_format());
    let mut pixel = vec![0.0; pixels.channels()];
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let coverage =
                if c.mode == GenerationMode::Component { 1.0 } else { c.selection.as_ref().map_or(1.0, |m| m.sample_channel(x, y, 0)).clamp(0.0, 1.0) };
            if coverage == 0.0 {
                continue;
            }
            resized.read_pixel(x - frame.x0, y - frame.y0, &mut pixel);
            if let Some(alpha) = pixel.last_mut() {
                *alpha *= coverage;
            }
            pixels.write_pixel(x, y, &pixel);
        }
    }
    let previous = s.focus();
    let recovered = destination.is_none();
    let select_result = recovered || c.is_current(s);
    let index = match destination {
        Some(index) => index,
        None => {
            let mut doc = Document::new("AI result", c.base.size, c.base.mode, c.base.depth);
            doc.icc_profile = c.base.icc_profile.clone();
            s.add_document(doc, None)
        }
    };
    if !s.set_active(index) {
        return Err(bad(ID, "destination document is unavailable"));
    }
    let added = s.edit("Add AI Layer", |doc, active| {
        let layer = photocraft_doc::Layer::new(doc.next_layer_name("AI"), LayerContent::Raster(pixels));
        let id = layer.id;
        let group_id = match c.group_id.filter(|gid| doc.ai_generations.iter().any(|g| g.id == *gid)) {
            Some(gid) => gid,
            None => {
                if doc.ai_generations.len() >= photocraft_doc::ai::MAX_GROUPS {
                    return Err(bad(ID, "AI history is full"));
                }
                let gid = doc.ai_generations.iter().map(|g| g.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| bad(ID, "AI group limit reached"))?;
                doc.ai_generations.push(photocraft_doc::ai::GenerationGroup {
                    id: gid, canvas: c.base.size, area: c.area, frame: c.frame, selection: c.selection.clone(), reference_png: c.source.clone(), mode: c.mode, context_png: c.context.clone(), versions: Vec::new(),
                });
                gid
            }
        };
        let group = doc.ai_generations.iter_mut().find(|g| g.id == group_id).ok_or_else(|| bad(ID, "missing AI group"))?;
        if group.versions.len() >= photocraft_doc::ai::MAX_VERSIONS {
            return Err(bad(ID, "AI version limit reached"));
        }
        let peers: Vec<_> = group.versions.iter().filter_map(|v| v.layer).collect();
        group.versions.push(photocraft_doc::ai::GenerationVersion {
            layer: Some(id), prompt: c.prompt.clone(), optimized_prompt: c.optimized_prompt.clone(), provider: c.provider.clone(), model: c.model.clone(),
        });
        for peer in peers {
            if let Some(layer) = doc.layer_mut(peer) { layer.visible = false; }
        }
        if doc.defold.is_some() {
            crate::defold_design::insert_artwork(doc, c.layer, layer)?;
        } else {
            doc.layers.push(layer);
        }
        if select_result {
            *active = Some(id);
        }
        Ok(json!({"candidateId":c.id,"groupId":group_id,"layerId":id.0,"documentId":doc.id.0,"name":doc.layer(id).map(|l| l.name.clone()),"savedAsLayer":true,"recovered":recovered,
            "placement":{"fit":if c.mode == GenerationMode::Component{"component"}else if c.frame.is_some(){"selection-context"}else{"contain"},"hasMargins":c.mode != GenerationMode::Component && c.frame.is_none() && area != c.area,"sourceSize":[image.size.width,image.size.height],"bounds":[area.x0,area.y0,area.width(),area.height()]}}))
    });
    if !recovered || matches!(previous, crate::defold_cmds::DocumentFocus::Gui(_)) {
        s.restore_focus(&previous);
    }
    let result = added?;
    s.ai.pending = None;
    s.ai.last_layer = Some(result.clone());
    Ok(result)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ai.beginGeneration",
            label: "Begin AI Generation",
            menu: &[],
            shortcut: None,
            params: r#"{"prompt":string,"provider":"images-api"|"codex","model":string="","reference":bool=false,"destination":"candidate"|"layer"="candidate","mode":"repaint"|"component"="repaint"}"#,
            enabled: target,
            run: begin,
            journal: false,
        },
        CommandSpec {
            id: "ai.receiveCandidate",
            label: "Receive AI Candidate",
            menu: &[],
            shortcut: None,
            params: r#"{"candidateId":integer,"imageBase64":string,"optimizedPrompt":string="","model":string?,"expandComponent":bool=false,"components":[{"name":string,"wide":bool=false}]?}"#,
            enabled: crate::commands::always,
            run: receive,
            journal: false,
        },
        CommandSpec {
            id: "ai.cancelGeneration",
            label: "Cancel AI Generation",
            menu: &[],
            shortcut: None,
            params: r#"{"candidateId":integer}"#,
            enabled: crate::commands::always,
            run: cancel,
            journal: false,
        },
        CommandSpec {
            id: "ai.prepareTestCandidate",
            label: "Create Test Candidate",
            menu: &[],
            shortcut: None,
            params: r#"{"prompt":string=""} // local-test: selection filled with foreground color"#,
            enabled: test_target,
            run: prepare,
            journal: false,
        },
        CommandSpec {
            id: "ai.applyCandidate",
            label: "Apply AI Candidate",
            menu: &[],
            shortcut: None,
            params: r#"{"candidateId":positive-integer}"#,
            enabled: crate::commands::always,
            run: apply,
            journal: true,
        },
        CommandSpec {
            id: "ai.discardCandidate",
            label: "Discard AI Candidate",
            menu: &[],
            shortcut: None,
            params: r#"{"candidateId":positive-integer}"#,
            enabled: crate::commands::always,
            run: discard,
            journal: false,
        },
        CommandSpec {
            id: "ai.inspect",
            label: "Inspect AI Candidate",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::commands::always,
            run: inspect,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "ai_cmds_tests.rs"]
mod tests;
