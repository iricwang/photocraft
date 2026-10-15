//! Task-scoped AI assets. Preview first, then add an atomic, revision-checked layer group.
use crate::{CommandSpec, EngineError, Result, Session};
use base64::Engine as _;
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Default)]
pub struct State {
    next: u64,
    task: Option<Task>,
}

struct Task {
    id: u64,
    base: Arc<Document>,
    revision: u64,
    context: String,
    detail: String,
    area: Rect,
    assets: Vec<Arc<Document>>,
    plan: Option<(u64, Layer)>,
    next_plan: u64,
    reserved: bool,
    receipt: Option<(String, Value)>,
}

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn task<'a>(s: &'a Session, p: &Value) -> Result<&'a Task> {
    let id = p.get("jobId").and_then(Value::as_u64).ok_or_else(|| error("jobId is required"))?;
    s.assets.task.as_ref().filter(|t| t.id == id).ok_or_else(|| error("Asset task is no longer active"))
}
fn decode(p: &Value) -> Result<Document> {
    let encoded = p.get("imageBase64").and_then(Value::as_str).filter(|s| s.len() <= 48 << 20).ok_or_else(|| error("Invalid asset image"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| error("Invalid asset base64"))?;
    let image = photocraft_codecs::decode_with(
        &bytes,
        &photocraft_codecs::DecodeOptions {
            limits: photocraft_codecs::Limits { max_width: 4096, max_height: 4096, max_pixels: 4 << 20, max_alloc: 64 << 20 },
            ..Default::default()
        },
    )
    .map_err(|e| error(e.to_string()))?;
    let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| error(e.to_string()))?;
    let mut doc = photocraft_io::import("asset.png", &png).map_err(|e| error(e.to_string()))?.document;
    if doc.icc_profile.is_none() {
        doc.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    }
    Ok(doc)
}
pub fn png(doc: &Document) -> Result<String> {
    let bytes = photocraft_io::export(doc, "asset.png", &photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() })
        .map_err(|e| error(e.to_string()))?
        .bytes;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn begin(s: &mut Session, _: &Value) -> Result<Value> {
    let st = s.active().ok_or_else(|| error("Open a document first"))?;
    if st.floating.is_some() || s.job_on(st.doc.id).is_some() {
        return Err(error("Finish the floating selection or current document operation first"));
    }
    let base = st.doc.clone();
    if base.size.width == 0 || base.size.height == 0 || u64::from(base.size.width) * u64::from(base.size.height) > 64 << 20 {
        return Err(error("Asset context canvas exceeds 64 megapixels"));
    }
    let revision = st.revision;
    let area = base.selection.as_ref().map(|mask| mask.content_bounds()).unwrap_or(base.bounds()).intersect(&base.bounds());
    if area.is_empty() {
        return Err(error("Selection is empty"));
    }
    let context_png = crate::ai_component::reference(&base, base.bounds(), 1024)?;
    let detail = crate::ai_component::reference(&base, area, 1536)?;
    let id = s.assets.next.checked_add(1).ok_or_else(|| error("Asset task limit reached"))?;
    s.assets.next = id;
    s.assets.task =
        Some(Task { id, base, revision, context: context_png, detail, area, assets: Vec::new(), plan: None, next_plan: 0, reserved: false, receipt: None });
    context(s, &json!({"jobId":id}))
}

fn context(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    let a = t.area;
    let mapping = |area: Rect, max: u32| {
        let scale = (f64::from(max) / f64::from(area.width().max(area.height()))).min(1.0);
        let width = (f64::from(area.width()) * scale).round().max(1.0) as u32;
        let height = (f64::from(area.height()) * scale).round().max(1.0) as u32;
        json!({"width":width,"height":height,"documentOrigin":[area.x0,area.y0],"documentPixelsPerPixel":[f64::from(area.width())/f64::from(width),f64::from(area.height())/f64::from(height)]})
    };
    Ok(json!({"jobId":t.id,"documentId":t.base.id.0,"revision":t.revision,"width":t.base.size.width,"height":t.base.size.height,
        "targetRect":[a.x0,a.y0,a.width(),a.height()],"imageBase64":t.context,"detailBase64":t.detail,
        "referenceMapping":mapping(t.base.bounds(),1024),"detailMapping":mapping(a,1536),"colorMode":t.base.mode,"sampleType":t.base.depth,
        "coordinates":"All target placements use original document pixels; reference previews may be downscaled.",
        "layers":t.base.layers.iter().map(|l| json!({"id":l.id.0,"name":l.name,"visible":l.visible})).collect::<Vec<_>>(),
        "sourcePolicy":"Original layers are never erased or hidden. Flattened controls can show beneath transparent overlays; report this rather than silently painting them out."}))
}

fn reserve(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    if t.reserved || !t.assets.is_empty() {
        return Err(error("The one-image budget is already spent; reuse the existing asset"));
    }
    if let Some(t) = s.assets.task.as_mut() {
        t.reserved = true;
    }
    Ok(json!({"reserved":true,"imageCallBudget":1}))
}

fn rebase(s: &mut Session, p: &Value) -> Result<Value> {
    let old = task(s, p)?;
    if old.receipt.is_some() || s.active().is_none_or(|st| st.doc.id != old.base.id) {
        return Err(error("Resume the original document before arranging retained assets"));
    }
    let old = s.assets.task.take();
    if let Err(err) = begin(s, &json!({})) {
        s.assets.task = old;
        return Err(err);
    }
    if let (Some(fresh), Some(old)) = (s.assets.task.as_mut(), old) {
        fresh.id = old.id;
        fresh.assets = old.assets;
        fresh.reserved = old.reserved;
        fresh.next_plan = old.next_plan;
    }
    context(s, p)
}

fn register(s: &mut Session, p: &Value) -> Result<Value> {
    if !task(s, p)?.assets.is_empty() {
        return Err(error("This task already has a source atlas"));
    }
    let doc = decode(p)?;
    let surface = doc.layers.first().and_then(Layer::surface).ok_or_else(|| error("Image has no pixels"))?;
    let alpha = surface.channels().saturating_sub(1);
    let mut clear = 0_u64;
    let mut visible = 0_u64;
    for y in 0..doc.size.height as i32 {
        for x in 0..doc.size.width as i32 {
            let a = surface.sample_channel(x, y, alpha);
            clear += u64::from(a <= 0.001);
            visible += u64::from(a > 0.1);
        }
    }
    if !surface.format().alpha || visible < 16 || clear * 100 < u64::from(doc.size.width) * u64::from(doc.size.height) {
        return Err(error("The asset set must have visible controls and real alpha transparency"));
    }
    if let Some(t) = s.assets.task.as_mut() {
        t.assets.push(Arc::new(doc));
    }
    inspect(s, &json!({"jobId":p["jobId"],"assetId":1}))
}

fn asset(t: &Task, id: u64) -> Result<&Document> {
    t.assets
        .get(id.checked_sub(1).and_then(|id| usize::try_from(id).ok()).ok_or_else(|| error("Invalid assetId"))?)
        .map(AsRef::as_ref)
        .ok_or_else(|| error("Asset is not part of this task"))
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    let id = p.get("assetId").and_then(Value::as_u64).unwrap_or(1);
    let doc = asset(t, id)?;
    let surface = doc.layers.first().and_then(Layer::surface).ok_or_else(|| error("Asset has no pixels"))?;
    let (w, h) = (doc.size.width as usize, doc.size.height as usize);
    let mask: Vec<bool> = (0..h).flat_map(|y| (0..w).map(move |x| surface.sample_channel(x as i32, y as i32, surface.channels() - 1) > 0.05)).collect();
    let (labels, sizes) = photocraft_algo::segment::components(&mask, w, h);
    let mut boxes = std::collections::BTreeMap::<u32, Rect>::new();
    for (i, label) in labels.iter().enumerate() {
        if *label == 0 || sizes.get(*label as usize).copied().unwrap_or(0) < 48 {
            continue;
        }
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        let r = boxes.entry(*label).or_insert(Rect::new(x, y, x + 1, y + 1));
        *r = Rect::new(r.x0.min(x), r.y0.min(y), r.x1.max(x + 1), r.y1.max(y + 1));
    }
    let mut boxes: Vec<_> = boxes.values().copied().collect();
    boxes.sort_by_key(|r| (r.y0, r.x0));
    Ok(
        json!({"assetId":id,"width":w,"height":h,"imageBase64":png(doc)?,"candidates":boxes.iter().take(64).map(|r| json!([r.x0,r.y0,r.width(),r.height()])).collect::<Vec<_>>(),
        "note":"Candidates are connected alpha regions, NOT semantic components. Inspect the actual image; merge separated pieces of the same control and add room for soft shadows. Rectangles use actual image pixels."}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Extraction {
    name: String,
    rect: [i32; 4],
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExtractParams {
    job_id: u64,
    asset_id: u64,
    items: Vec<Extraction>,
}

fn extract(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    if p.get("items").and_then(Value::as_array).is_none_or(|v| v.is_empty() || v.len() > 24) {
        return Err(error("Extract 1 to 24 components"));
    }
    let params: ExtractParams = serde_json::from_value(p.clone()).map_err(|e| error(e.to_string()))?;
    if params.job_id != t.id || params.asset_id != 1 {
        return Err(error("Extract only from the original task atlas"));
    }
    if t.assets.len() + params.items.len() > 49 {
        return Err(error("Asset cache is full; start a new task to clear it"));
    }
    let source = asset(t, params.asset_id)?;
    let pixels = source.layers.first().and_then(Layer::surface).ok_or_else(|| error("Missing pixels"))?;
    let mut names = std::collections::BTreeSet::new();
    let mut regions = Vec::new();
    let mut docs = Vec::new();
    let mut metadata = Vec::new();
    for item in params.items {
        if item.name.trim().is_empty() || item.name.len() > 192 || !names.insert(item.name.clone()) {
            return Err(error("Use unique component names of at most 192 bytes"));
        }
        let [x, y, w, h] = item.rect;
        if w <= 0 || h <= 0 || x < 0 || y < 0 {
            return Err(error("Invalid extraction rectangle"));
        }
        let rect = Rect::new(x, y, x.checked_add(w).ok_or_else(|| error("Rectangle overflow"))?, y.checked_add(h).ok_or_else(|| error("Rectangle overflow"))?);
        if rect.intersect(&source.bounds()) != rect {
            return Err(error("Extraction rectangle is outside the actual atlas"));
        }
        for prev in &regions {
            let overlap = rect.intersect(prev);
            for y in overlap.y0..overlap.y1 {
                for x in overlap.x0..overlap.x1 {
                    if pixels.sample_channel(x, y, pixels.channels() - 1) > 0.03 {
                        return Err(error("Extraction rectangles share visible pixels; inspect and separate the controls"));
                    }
                }
            }
        }
        let mut tight = Rect::new(rect.x1, rect.y1, rect.x0, rect.y0);
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                if pixels.sample_channel(x, y, pixels.channels() - 1) > 0.001 {
                    tight = Rect::new(tight.x0.min(x), tight.y0.min(y), tight.x1.max(x + 1), tight.y1.max(y + 1));
                }
            }
        }
        if tight.is_empty() {
            return Err(error(format!("Component {} contains no visible pixels", item.name)));
        }
        // A substantial edge pixel means the proposed rectangle cuts through the component.
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                if (x == rect.x0 || x == rect.x1 - 1 || y == rect.y0 || y == rect.y1 - 1) && pixels.sample_channel(x, y, pixels.channels() - 1) > 0.05 {
                    return Err(error(format!("Component {} touches its crop edge; expand or correct the region", item.name)));
                }
            }
        }
        tight = Rect::new((tight.x0 - 2).max(rect.x0), (tight.y0 - 2).max(rect.y0), (tight.x1 + 2).min(rect.x1), (tight.y1 + 2).min(rect.y1));
        let mut doc = Document::new(&item.name, Size::new(tight.width(), tight.height()), source.mode, source.depth);
        doc.icc_profile = source.icc_profile.clone();
        let mut out = Surface::new(pixels.format());
        out.write_region(doc.bounds(), &pixels.read_region(tight));
        doc.layers.push(Layer::new(&item.name, LayerContent::Raster(out)));
        let id = t.assets.len() + docs.len() + 1;
        metadata.push(
            json!({"assetId":id,"name":item.name,"width":tight.width(),"height":tight.height(),"sourceRect":[tight.x0,tight.y0,tight.width(),tight.height()]}),
        );
        docs.push(Arc::new(doc));
        regions.push(rect);
    }
    let total: u64 = t.assets.iter().chain(docs.iter()).map(|doc| u64::from(doc.size.width) * u64::from(doc.size.height)).sum();
    if total > 16 << 20 {
        return Err(error("Task assets exceed 16 megapixels; reuse existing extracted assets"));
    }
    if let Some(t) = s.assets.task.as_mut() {
        t.assets.extend(docs);
        t.plan = None;
    }
    Ok(json!({"assets":metadata}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Placement {
    asset_id: u64,
    name: String,
    x: i32,
    y: i32,
    width: u32,
    visible: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewParams {
    job_id: u64,
    items: Vec<Placement>,
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    if p.get("items").and_then(Value::as_array).is_none_or(|v| v.is_empty() || v.len() > 24) {
        return Err(error("Place 1 to 24 assets"));
    }
    let params: PreviewParams = serde_json::from_value(p.clone()).map_err(|e| error(e.to_string()))?;
    if params.job_id != t.id {
        return Err(error("Wrong task"));
    }
    let mut children = Vec::new();
    let mut placements = Vec::new();
    let mut total = 0_u64;
    for item in params.items {
        if item.name.is_empty() || item.name.len() > 192 || item.width == 0 || item.width > 8192 || item.x < 0 || item.y < 0 {
            return Err(error("Invalid asset placement"));
        }
        let mut doc = asset(t, item.asset_id)?.clone();
        crate::color_cmds::convert_document(&mut doc, &crate::color_cmds::document_profile(&t.base), photocraft_cms::Intent::RelativeColorimetric, true)?;
        let scale = f64::from(item.width) / f64::from(doc.size.width);
        let height = (f64::from(doc.size.height) * scale).round().max(1.0) as u32;
        total = total.saturating_add(u64::from(item.width) * u64::from(height));
        if height > 8192
            || total > 16 << 20
            || u64::from(item.width) + item.x as u64 > u64::from(t.base.size.width)
            || u64::from(height) + item.y as u64 > u64::from(t.base.size.height)
        {
            return Err(error("Placement exceeds canvas or pixel budget"));
        }
        let pixels = doc.layers.first().and_then(Layer::surface).ok_or_else(|| error("Missing asset pixels"))?;
        let resized = photocraft_algo::resample::resize_surface_in_canvas(pixels, scale, scale, photocraft_algo::resample::Resample::Bicubic, doc.bounds())
            .convert(t.base.pixel_format());
        let mut layer = Layer::new(&item.name, LayerContent::Raster(resized.translated(item.x, item.y, Rect::new(0, 0, item.width as i32, height as i32))));
        layer.visible = item.visible;
        placements.push(json!({"name":item.name,"assetId":item.asset_id,"rect":[item.x,item.y,item.width,height],"visible":item.visible}));
        children.push(layer);
    }
    let layer = Layer::group("AI assets", children);
    let mut doc = (*t.base).clone();
    doc.layers.push(layer.clone());
    let encoded = crate::ai_component::reference(&doc, doc.bounds(), 1536)?;
    let id = t.next_plan.checked_add(1).ok_or_else(|| error("Plan limit reached"))?;
    let revision = t.revision;
    if let Some(t) = s.assets.task.as_mut() {
        t.next_plan = id;
        t.plan = Some((id, layer));
    }
    Ok(
        json!({"planId":id,"expectedRevision":revision,"placements":placements,"imageBase64":encoded,"warning":"Source pixels are preserved. Inspect overlap, old flattened controls, text occlusion and mutually exclusive states before applying."}),
    )
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    let key = p
        .get("idempotencyKey")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or_else(|| error("idempotencyKey is required"))?
        .to_owned();
    if let Some((old, receipt)) = &t.receipt {
        return if *old == key { Ok(receipt.clone()) } else { Err(error("This task has already been applied")) };
    }
    let (plan_id, layer) = t
        .plan
        .as_ref()
        .filter(|(id, _)| Some(*id) == p.get("planId").and_then(Value::as_u64))
        .ok_or_else(|| error("Render a valid placement preview first"))?;
    if Some(t.revision) != p.get("expectedRevision").and_then(Value::as_u64) {
        return Err(error("Snapshot revision mismatch"));
    }
    let index = s
        .documents()
        .iter()
        .position(|st| st.doc.id == t.base.id && st.revision == t.revision && Arc::ptr_eq(&st.doc, &t.base) && st.floating.is_none())
        .ok_or_else(|| error("The source document changed or closed; assets are retained, no layers were added"))?;
    if s.job_on(t.base.id).is_some() {
        return Err(error("The source document is busy"));
    }
    let layer = layer.clone();
    let job = t.id;
    let plan = *plan_id;
    let previous = s.focus();
    s.set_active(index);
    let receipt = s.edit("Add AI Assets", |doc, active| {
        let id = layer.id;
        let count = layer.children().map_or(0, |c| c.len());
        doc.layers.push(layer);
        *active = Some(id);
        Ok(json!({"jobId":job,"planId":plan,"documentId":doc.id.0,"layerId":id.0,"name":"AI assets","componentCount":count,"savedAsLayer":true}))
    });
    s.restore_focus(&previous);
    let receipt = receipt?;
    if let Some(t) = s.assets.task.as_mut() {
        t.receipt = Some((key, receipt.clone()));
    }
    Ok(receipt)
}
fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let t = task(s, p)?;
    Ok(
        json!({"jobId":t.id,"documentId":t.base.id.0,"imageCalls":u8::from(t.reserved),"assetCount":t.assets.len(),"assets":t.assets.iter().enumerate().map(|(i,d)|json!({"assetId":i+1,"name":d.name,"width":d.size.width,"height":d.size.height})).collect::<Vec<_>>(),"receipt":t.receipt.as_ref().map(|(_,v)|v)}),
    )
}
fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    task(s, p)?;
    s.assets.task = None;
    Ok(json!({"cancelled":true}))
}

pub fn specs() -> Vec<CommandSpec> {
    [
        ("ai.assetBegin", "Begin Asset Task", begin as fn(&mut Session, &Value) -> Result<Value>),
        ("ai.assetRebase", "Resume Asset Task", rebase),
        ("ai.designContext", "Read Design Context", context),
        ("ai.assetReserve", "Reserve Image Budget", reserve),
        ("ai.assetRegister", "Register Generated Asset", register),
        ("ai.assetInspect", "Inspect Generated Asset", inspect),
        ("ai.assetExtract", "Extract Asset Components", extract),
        ("ai.layersPreview", "Preview Asset Layers", preview),
        ("ai.layersApply", "Apply Asset Layers", apply),
        ("ai.assetStatus", "Read Asset Task", status),
        ("ai.assetCancel", "Cancel Asset Task", cancel),
    ]
    .into_iter()
    .map(|(id, label, run)| CommandSpec {
        id,
        label,
        run,
        menu: &[],
        shortcut: None,
        params: match id {
            "ai.assetBegin" => "{}; capture the active document and optional selection",
            "ai.assetRegister" => "{jobId:u64,imageBase64:string}; host-only generated PNG, max 4 MP",
            "ai.assetInspect" => "{jobId:u64,assetId?:u64}; default atlas assetId=1",
            "ai.assetExtract" => "{jobId:u64,assetId:1,items:[{name:string,rect:[x,y,width,height]}]}; actual atlas pixels, 1-24 items",
            "ai.layersPreview" => {
                "{jobId:u64,items:[{assetId:u64,name:string,x:i32,y:i32,width:u32,visible:bool}]}; document pixels, uniform scale, 1-24 items"
            }
            "ai.layersApply" => "{jobId:u64,planId:u64,expectedRevision:u64,idempotencyKey:string}; atomic one-undo group",
            _ => "{jobId:u64}; task-scoped, see docs/ai-integration.md",
        },
        enabled: crate::commands::always,
        journal: false,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Session, Value) {
        setup_depth(8)
    }
    fn setup_depth(depth: u8) -> (Session, Value) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":320,"height":160,"depth":depth})).unwrap();
        let context = s.execute("ai.assetBegin", json!({})).unwrap();
        let mut doc = Document::new("Test", Size::new(128, 64), photocraft_doc::ColorMode::Rgb, photocraft_color::SampleType::U8);
        let mut pixels = Surface::new(doc.pixel_format());
        pixels.fill_rect(Rect::new(16, 16, 48, 48), &[0.2, 0.8, 0.3, 1.0]);
        pixels.fill_rect(Rect::new(80, 16, 112, 48), &[0.8, 0.2, 0.3, 0.5]);
        doc.layers.push(Layer::new("Atlas", LayerContent::Raster(pixels)));
        s.execute("ai.assetRegister", json!({"jobId":context["jobId"],"imageBase64":png(&doc).unwrap()})).unwrap();
        (s, context)
    }
    fn extracted(s: &mut Session, context: &Value) -> Value {
        s.execute(
            "ai.assetExtract",
            json!({"jobId":context["jobId"],"assetId":1,"items":[{"name":"Play","rect":[8,8,48,48]},{"name":"Pause","rect":[72,8,48,48]}]}),
        )
        .unwrap()
    }
    fn planned(s: &mut Session, context: &Value) -> Value {
        s.execute("ai.layersPreview",json!({"jobId":context["jobId"],"items":[{"assetId":2,"name":"Play","x":100,"y":90,"width":40,"visible":false},{"assetId":3,"name":"Pause","x":100,"y":90,"width":40,"visible":true}]})).unwrap()
    }
    #[test]
    fn asset_layers_keep_document_depth_and_transparency() {
        for depth in [8, 16, 32] {
            let (mut s, c) = setup_depth(depth);
            extracted(&mut s, &c);
            let plan = planned(&mut s, &c);
            s.execute(
                "ai.layersApply",
                json!({"jobId":c["jobId"],"planId":plan["planId"],"expectedRevision":plan["expectedRevision"],"idempotencyKey":"depth"}),
            )
            .unwrap();
            let doc = &s.active().unwrap().doc;
            let surface = doc.layers.last().unwrap().children().unwrap()[1].surface().unwrap();
            assert_eq!(surface.format(), doc.pixel_format());
            assert!((surface.rgba(110, 100)[3] - 0.5).abs() < 0.01);
            assert_eq!(surface.rgba(0, 0)[3], 0.0);
        }
    }
    #[test]
    fn actual_regions_create_isolated_layers_and_atomic_idempotent_history() {
        let (mut s, c) = setup();
        let original = s.active().unwrap().doc.clone();
        let crops = extracted(&mut s, &c);
        assert_eq!(crops["assets"][0]["width"], 36);
        let job = s.assets.task.as_ref().unwrap();
        assert_eq!(asset(job, 2).unwrap().layers[0].surface().unwrap().rgba(66, 2)[3], 0.0);
        let plan = planned(&mut s, &c);
        assert!(Arc::ptr_eq(&original, &s.active().unwrap().doc));
        let params = json!({"jobId":c["jobId"],"planId":plan["planId"],"expectedRevision":plan["expectedRevision"],"idempotencyKey":"batch-one"});
        let one = s.execute("ai.layersApply", params.clone()).unwrap();
        let two = s.execute("ai.layersApply", params).unwrap();
        assert_eq!(one, two);
        let doc = &s.active().unwrap().doc;
        assert_eq!(doc.layers.len(), original.layers.len() + 1);
        let group = doc.layers.last().unwrap().children().unwrap();
        assert!(!group[0].visible);
        assert!(group[1].visible);
        assert!(group[1].surface().unwrap().rgba(104, 94)[3] > 0.4);
        let native = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
        let restored = photocraft_format::load_from_bytes(&native).unwrap();
        assert_eq!(restored.layers.last().unwrap().children().unwrap().len(), 2);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len());
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len() + 1);
        assert!(s.execute("ai.assetReserve", json!({"jobId":c["jobId"]})).is_err());
    }
    #[test]
    fn malformed_crops_stale_source_and_budget_fail_without_mutation() {
        let (mut s, c) = setup();
        let before = s.active().unwrap().doc.clone();
        for rect in [[-1, 0, 20, 20], [0, 0, i32::MAX, 50], [0, 0, 8, 8], [16, 16, 32, 32]] {
            assert!(s.execute("ai.assetExtract", json!({"jobId":c["jobId"],"assetId":1,"items":[{"name":"Broken","rect":rect}]})).is_err());
            assert_eq!(s.assets.task.as_ref().unwrap().assets.len(), 1);
        }
        assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
        extracted(&mut s, &c);
        let plan = planned(&mut s, &c);
        s.execute("layer.new.layer", json!({"name":"User work"})).unwrap();
        let changed = s.active().unwrap().doc.clone();
        assert!(
            s.execute("ai.layersApply", json!({"jobId":c["jobId"],"planId":plan["planId"],"expectedRevision":plan["expectedRevision"],"idempotencyKey":"one"}))
                .is_err()
        );
        assert!(Arc::ptr_eq(&changed, &s.active().unwrap().doc));
        s.execute("ai.assetRebase", json!({"jobId":c["jobId"]})).unwrap();
        assert_eq!(s.execute("ai.assetStatus", json!({"jobId":c["jobId"]})).unwrap()["assetCount"], 3);
        assert!(s.execute("ai.assetReserve", json!({"jobId":c["jobId"]})).is_err());
        assert!(
            s.execute(
                "ai.layersApply",
                json!({"jobId":c["jobId"],"planId":plan["planId"],"expectedRevision":plan["expectedRevision"],"idempotencyKey":"stale"})
            )
            .is_err()
        );
        let fresh_plan = planned(&mut s, &c);
        s.execute(
            "ai.layersApply",
            json!({"jobId":c["jobId"],"planId":fresh_plan["planId"],"expectedRevision":fresh_plan["expectedRevision"],"idempotencyKey":"resumed"}),
        )
        .unwrap();
        let fresh = s.execute("ai.assetBegin", json!({})).unwrap();
        assert!(s.execute("ai.assetStatus", json!({"jobId":c["jobId"]})).is_err());
        s.execute("ai.assetReserve", json!({"jobId":fresh["jobId"]})).unwrap();
        assert!(s.execute("ai.assetReserve", json!({"jobId":fresh["jobId"]})).is_err());
    }
}
