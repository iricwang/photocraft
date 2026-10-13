//! Isolated RGBA assets: two references and aspect-preserving placement, never a scene crop.
use base64::Engine as _;
use photocraft_color::{PixelFormat, SampleType};
use photocraft_doc::{ColorMode, Document, Layer, LayerContent};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;

use crate::{EngineError, Result};

pub(super) fn reference(doc: &Document, area: Rect, max_side: u32) -> Result<String> {
    let scale = (f64::from(max_side) / f64::from(area.width().max(area.height()).max(1))).min(1.0);
    let w = (f64::from(area.width()) * scale).round().max(1.0) as u32;
    let h = (f64::from(area.height()) * scale).round().max(1.0) as u32;
    let mut rgb = doc.clone();
    crate::color_cmds::convert_document(&mut rgb, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let rendered = photocraft_compose::render_reduced_rect(&rgb, area, w, h);
    let mut surface = Surface::new(PixelFormat::RGBA8.with_sample(SampleType::F32));
    for (i, pixel) in rendered.px.iter().enumerate() {
        surface.write_pixel((i % w as usize) as i32, (i / w as usize) as i32, pixel);
    }
    let mut reference = Document::new("Component reference", Size::new(w, h), ColorMode::Rgb, SampleType::F32);
    reference.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    reference.layers.push(Layer::new("Reference", LayerContent::Raster(surface)));
    let bytes = photocraft_io::export(&reference, "reference.png", &photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() })
        .map_err(|e| EngineError::Other(e.to_string()))?
        .bytes;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub(super) fn instructions(area: Rect, canvas: Size) -> String {
    format!(
        "Selected component location in the full interface (origin top-left): x={:.3}%..{:.3}%, y={:.3}%..{:.3}%. \
        Original canvas: {} x {} pixels; selected region: {} x {} pixels. \
        These coordinates IDENTIFY the component in context; they are not output dimensions or a silhouette constraint.",
        f64::from(area.x0) * 100.0 / f64::from(canvas.width.max(1)),
        f64::from(area.x1) * 100.0 / f64::from(canvas.width.max(1)),
        f64::from(area.y0) * 100.0 / f64::from(canvas.height.max(1)),
        f64::from(area.y1) * 100.0 / f64::from(canvas.height.max(1)),
        canvas.width,
        canvas.height,
        area.width(),
        area.height()
    )
}

pub(super) fn mapped_result(image: &Document, area: Rect, canvas: Rect, format: PixelFormat) -> Result<(Surface, Rect)> {
    let source = image.layers.first().and_then(|l| l.surface()).ok_or_else(|| EngineError::Other("Image contains no pixels".into()))?;
    let error = || {
        EngineError::Other("The provider did not return a usable transparent component. Request a PNG with real transparency or choose a model that supports transparent output.".into())
    };
    if !source.format().alpha {
        return Err(error());
    }
    let alpha = source.channels().saturating_sub(1);
    let mut transparent = 0u64;
    let mut peak_alpha = 0.0f32;
    for y in 0..image.size.height as i32 {
        for x in 0..image.size.width as i32 {
            let a = source.sample_channel(x, y, alpha);
            peak_alpha = peak_alpha.max(a);
            if a <= 0.001 {
                transparent += 1;
            }
        }
    }
    if peak_alpha <= 0.001 || transparent * 100 < u64::from(image.size.width) * u64::from(image.size.height) {
        return Err(error());
    }
    // Near-invisible alpha dust must not make a narrow component fit the entire image canvas.
    // Use a relative threshold so translucent components still retain their own silhouette.
    let mut bounds = Rect::new(image.size.width as i32, image.size.height as i32, 0, 0);
    let mut body = bounds;
    for y in 0..image.size.height as i32 {
        for x in 0..image.size.width as i32 {
            let a = source.sample_channel(x, y, alpha);
            if a > peak_alpha * 0.01 {
                bounds.x0 = bounds.x0.min(x);
                bounds.y0 = bounds.y0.min(y);
                bounds.x1 = bounds.x1.max(x + 1);
                bounds.y1 = bounds.y1.max(y + 1);
            }
            if a >= peak_alpha * 0.5 {
                body.x0 = body.x0.min(x);
                body.y0 = body.y0.min(y);
                body.x1 = body.x1.max(x + 1);
                body.y1 = body.y1.max(y + 1);
            }
        }
    }
    // Long bars keep their major-axis size; allow up to 2x minor-axis growth for new raised controls.
    // Anchor canvas-edge components to that edge, and retain the original selection in history.
    let mut area = area;
    if area.width() >= area.height().saturating_mul(2) {
        let height = (f64::from(body.height()) * f64::from(area.width()) / f64::from(body.width())).ceil() as u32;
        let height = height.max(area.height()).min(area.height().saturating_mul(2)).min(canvas.height()).min(16 * 1024 * 1024 / area.width().max(1)) as i32;
        let y = if area.y1 == canvas.y1 { canvas.y1 - height } else { (area.y0 + (area.height() as i32 - height) / 2).clamp(canvas.y0, canvas.y1 - height) };
        area = Rect::from_xywh(area.x0, y, area.width(), height as u32);
    } else if area.height() >= area.width().saturating_mul(2) {
        let width = (f64::from(body.width()) * f64::from(area.height()) / f64::from(body.height())).ceil() as u32;
        let width = width.max(area.width()).min(area.width().saturating_mul(2)).min(canvas.width()).min(16 * 1024 * 1024 / area.height().max(1)) as i32;
        let x = if area.x1 == canvas.x1 { canvas.x1 - width } else { (area.x0 + (area.width() as i32 - width) / 2).clamp(canvas.x0, canvas.x1 - width) };
        area = Rect::from_xywh(x, area.y0, width as u32, area.height());
    }
    // Fit the body, not its soft shadow; retain the surrounding alpha within the placement area.
    let scale = (f64::from(area.width()) / f64::from(body.width())).min(f64::from(area.height()) / f64::from(body.height()));
    let pad_x = ((f64::from(area.width()) / scale - f64::from(body.width())) / 2.0).floor().max(0.0) as i32;
    let pad_y = ((f64::from(area.height()) / scale - f64::from(body.height())) / 2.0).floor().max(0.0) as i32;
    // Discard off-placement margins before scaling, bounding allocation even for a tiny subject.
    bounds = bounds.intersect(&Rect::new(body.x0 - pad_x, body.y0 - pad_y, body.x1 + pad_x, body.y1 + pad_y));
    let mut cropped = Surface::new(source.format());
    let origin = Rect::from_xywh(0, 0, bounds.width(), bounds.height());
    cropped.write_region(origin, &source.read_region(bounds));
    let resized =
        photocraft_algo::resample::resize_surface_in_canvas(&cropped, scale, scale, photocraft_algo::resample::Resample::Bicubic, origin).convert(format);
    let offset_x = ((f64::from(area.width()) - f64::from(body.width()) * scale) / 2.0 - f64::from(body.x0 - bounds.x0) * scale).round() as i32;
    let offset_y = ((f64::from(area.height()) - f64::from(body.height()) * scale) / 2.0 - f64::from(body.y0 - bounds.y0) * scale).round() as i32;
    let mut placed = Surface::new(format);
    let mut pixel = vec![0.0; placed.channels()];
    for y in 0..area.height() as i32 {
        for x in 0..area.width() as i32 {
            resized.read_pixel(x - offset_x, y - offset_y, &mut pixel);
            placed.write_pixel(x, y, &pixel);
        }
    }
    Ok((placed, area))
}
