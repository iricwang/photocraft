//! One transparent atlas, locally split into bounded, independently reusable assets.
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSpec {
    pub name: String,
    #[serde(default)]
    pub wide: bool,
}

pub fn parse(buttons: &str, bars: &str) -> Result<Vec<AssetSpec>, String> {
    if buttons.len().saturating_add(bars.len()) > 4096 {
        return Err("Component list is too long".into());
    }
    let specs: Vec<_> = [(buttons, false), (bars, true)]
        .into_iter()
        .flat_map(|(text, wide)| text.lines().map(str::trim).filter(|name| !name.is_empty()).map(move |name| AssetSpec { name: name.into(), wide }))
        .collect();
    validate(&specs)?;
    Ok(specs)
}

pub fn validate(specs: &[AssetSpec]) -> Result<(), String> {
    if !(2..=12).contains(&specs.len()) {
        return Err("Choose between 2 and 12 components".into());
    }
    if specs.iter().filter(|s| s.wide).count() > 4 {
        return Err("Choose at most 4 tracks per component set".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for spec in specs {
        if spec.name.trim().is_empty() || spec.name.len() > 192 || spec.name.chars().count() > 64 || spec.name.chars().any(char::is_control) {
            return Err("Component names must contain 1 to 64 characters".into());
        }
        if !names.insert(spec.name.trim().to_lowercase()) {
            return Err("Component names must be unique (include the state in each name)".into());
        }
    }
    Ok(())
}

/// Integer grid coordinates, independent of the image provider's actual resolution.
pub fn layout(specs: &[AssetSpec]) -> Result<(u32, u32, Vec<Rect>), String> {
    validate(specs)?;
    let buttons = specs.iter().filter(|s| !s.wide).count() as u32;
    let columns = if buttons > 4 { 4 } else { 2 };
    let button_rows = buttons.div_ceil(columns);
    let rows = button_rows + specs.iter().filter(|s| s.wide).count() as u32;
    let (mut button, mut bar) = (0, button_rows);
    let cells = specs
        .iter()
        .map(|spec| {
            if spec.wide {
                let cell = Rect::new(0, bar as i32, columns as i32, bar as i32 + 1);
                bar += 1;
                cell
            } else {
                let (x, y) = ((button % columns) as i32, (button / columns) as i32);
                button += 1;
                Rect::new(x, y, x + 1, y + 1)
            }
        })
        .collect();
    Ok((columns, rows, cells))
}

pub fn instructions(specs: &[AssetSpec]) -> Result<String, String> {
    let (columns, rows, cells) = layout(specs)?;
    let mut text = format!(
        "Generate one PNG with true alpha transparency containing the {} requested assets listed below. \
         Follow the user's request and the supplied reference image roles. \
         For local extraction, keep each complete asset inside its assigned rectangle with transparent space around its visible edges. \
         Do not draw the rectangle boundaries or add labels not requested by the user. \
         Coordinates are percentages of the output canvas, origin top-left.\n",
        specs.len()
    );
    for (spec, cell) in specs.iter().zip(cells) {
        text.push_str(&format!(
            "- {}: x={:.2}..{:.2}%, y={:.2}..{:.2}%.\n",
            serde_json::to_string(&spec.name).map_err(|e| e.to_string())?,
            100.0 * f64::from(cell.x0) / f64::from(columns),
            100.0 * f64::from(cell.x1) / f64::from(columns),
            100.0 * f64::from(cell.y0) / f64::from(rows),
            100.0 * f64::from(cell.y1) / f64::from(rows),
        ));
    }
    Ok(text)
}

#[derive(Debug)]
pub struct Asset {
    pub name: String,
    pub bounds: Rect,
    /// Tight local pixels, retaining transparent edge padding.
    pub pixels: Surface,
}

impl Asset {
    pub fn document(&self, source: &Document) -> Document {
        let mut doc = Document::new(&self.name, photocraft_geom::Size::new(self.bounds.width(), self.bounds.height()), source.mode, source.depth);
        doc.icc_profile = source.icc_profile.clone();
        doc.layers.push(Layer::new(&self.name, LayerContent::Raster(self.pixels.clone())));
        doc
    }

    pub fn layer(&self) -> Layer {
        let bounds = Rect::new(0, 0, self.bounds.width() as i32, self.bounds.height() as i32);
        Layer::new(&self.name, LayerContent::Raster(self.pixels.translated(self.bounds.x0, self.bounds.y0, bounds)))
    }
}

pub fn split(image: &Document, specs: &[AssetSpec]) -> Result<Vec<Asset>, String> {
    let (columns, rows, cells) = layout(specs)?;
    let (width, height) = (image.size.width, image.size.height);
    if width < 32 * columns || height < 32 * rows || u64::from(width) * u64::from(height) > 4 * 1024 * 1024 {
        return Err("Component atlas resolution is outside the supported range".into());
    }
    let aspect = f64::from(width) * f64::from(rows) / (f64::from(height) * f64::from(columns));
    if !(0.8..=1.25).contains(&aspect) {
        return Err("Component atlas has the wrong aspect ratio; the original image has been retained".into());
    }
    let surface = image.layers.first().and_then(Layer::surface).ok_or("Component atlas has no raster pixels")?;
    let format = surface.format();
    if !format.alpha {
        return Err("Component atlas needs a real transparent background".into());
    }
    let alpha = format.channels() - 1;
    let mut assets = Vec::new();
    for (spec, cell) in specs.iter().zip(cells) {
        let cell = Rect::new(
            (cell.x0 as u32 * width / columns) as i32,
            (cell.y0 as u32 * height / rows) as i32,
            (cell.x1 as u32 * width / columns) as i32,
            (cell.y1 as u32 * height / rows) as i32,
        );
        let (gx, gy) = ((cell.width() / 50).max(2) as i32, (cell.height() / 50).max(2) as i32);
        let mut bounds = Rect::new(cell.x1, cell.y1, cell.x0, cell.y0);
        let mut visible = 0;
        let mut transparent = 0_u64;
        let mut peak = 0.0_f32;
        for y in cell.y0..cell.y1 {
            for x in cell.x0..cell.x1 {
                let a = surface.sample_channel(x, y, alpha);
                peak = peak.max(a);
                if a <= 0.001 {
                    transparent += 1;
                }
                if a > 0.03 && (x < cell.x0 + gx || x >= cell.x1 - gx || y < cell.y0 + gy || y >= cell.y1 - gy) {
                    return Err(format!(
                        "Component '{}' crosses its cell boundary or has an opaque background; the atlas was retained, no automatic regeneration",
                        spec.name
                    ));
                }
                if a > 0.001 {
                    visible += 1;
                    bounds = Rect::new(bounds.x0.min(x), bounds.y0.min(y), bounds.x1.max(x + 1), bounds.y1.max(y + 1));
                }
            }
        }
        if visible < 16 || bounds.is_empty() || peak < 0.1 {
            return Err(format!("Component '{}' is empty; the atlas was retained, no automatic regeneration", spec.name));
        }
        if transparent * 100 < u64::from(cell.width()) * u64::from(cell.height()) * 5 {
            return Err(format!("Component '{}' needs a real transparent background; the atlas was retained", spec.name));
        }
        bounds = Rect::new((bounds.x0 - 2).max(cell.x0), (bounds.y0 - 2).max(cell.y0), (bounds.x1 + 2).min(cell.x1), (bounds.y1 + 2).min(cell.y1));
        // `translated` retains complete tiles, including neighboring sprites. Copy only this crop.
        let mut pixels = Surface::new(format);
        pixels.write_region(Rect::new(0, 0, bounds.width() as i32, bounds.height() as i32), &surface.read_region(bounds));
        assets.push(Asset { name: spec.name.clone(), bounds, pixels });
    }
    Ok(assets)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fixture(specs: &[AssetSpec]) -> Document {
        let (cols, rows, cells) = layout(specs).unwrap();
        let mut doc =
            Document::new("Atlas", photocraft_geom::Size::new(cols * 64, rows * 64), photocraft_doc::ColorMode::Rgb, photocraft_color::SampleType::U8);
        let mut surface = Surface::new(doc.pixel_format());
        for cell in cells {
            for y in cell.y0 * 64 + 16..cell.y1 * 64 - 16 {
                for x in cell.x0 * 64 + 16..cell.x1 * 64 - 16 {
                    surface.write_pixel(x, y, &[0.3, 0.8, 0.2, 1.0]);
                }
            }
        }
        doc.layers.push(Layer::new("Atlas", LayerContent::Raster(surface)));
        doc
    }

    #[test]
    fn asset_prompt_keeps_requested_names_and_only_required_extraction_geometry() {
        let specs = parse("Cover artwork\nStatus icon", "Playback bar").unwrap();
        let prompt = instructions(&specs).unwrap();
        assert!(prompt.contains("true alpha transparency"));
        assert!(prompt.contains("assigned rectangle"));
        for name in ["Cover artwork", "Status icon", "Playback bar"] {
            assert!(prompt.contains(&serde_json::to_string(name).unwrap()));
        }
        let (columns, rows, cells) = layout(&specs).unwrap();
        for cell in cells {
            assert!(prompt.contains(&format!(
                "x={:.2}..{:.2}%, y={:.2}..{:.2}%",
                100.0 * f64::from(cell.x0) / f64::from(columns),
                100.0 * f64::from(cell.x1) / f64::from(columns),
                100.0 * f64::from(cell.y0) / f64::from(rows),
                100.0 * f64::from(cell.y1) / f64::from(rows),
            )));
        }
        for extra in ["3D", "bevel", "material", "lighting", "front-facing", "grid", "12%", "slender", "one isolated button", "fill and thumb"] {
            assert!(!prompt.contains(extra), "unrequested direction: {extra}");
        }
    }

    #[test]
    fn atlas_layout_split_preserves_local_pixels_and_names() {
        let specs = parse("Play\nPause\nPrevious\nNext\nShuffle\nRepeat\nLike\nVolume", "Progress\nVolume track").unwrap();
        assert_eq!(layout(&specs).unwrap().0, 4);
        let image = fixture(&specs);
        let assets = split(&image, &specs).unwrap();
        assert_eq!(assets.len(), 10);
        assert_eq!(assets[1].name, "Pause");
        assert!(assets[8].bounds.width() > assets[0].bounds.width() * 4);
        assert_eq!(assets[0].pixels.sample_channel(2, 2, 3), 1.0);
        assert_eq!(assets[0].pixels.sample_channel(0, 0, 3), 0.0);
        assert_eq!(assets[1].layer().surface().unwrap().sample_channel(80, 16, 3), 1.0);
        assert_eq!(assets[0].layer().surface().unwrap().sample_channel(80, 16, 3), 0.0, "neighboring sprites must not leak through shared tiles");
        assert_eq!(assets[1].layer().surface().unwrap().sample_channel(16, 16, 3), 0.0);
        assert_eq!(assets[1].document(&image).size.width, 36);
    }

    #[test]
    fn invalid_lists_empty_cells_and_crossing_assets_are_rejected() {
        for (buttons, bars) in [("", ""), ("Play", ""), ("Play\nplay", ""), ("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm", "")] {
            assert!(parse(buttons, bars).is_err());
        }
        let specs = parse("Play\nPause", "").unwrap();
        let mut image = fixture(&specs);
        image.layers[0].surface_mut().unwrap().write_pixel(64, 30, &[1.0; 4]);
        assert!(split(&image, &specs).unwrap_err().contains("boundary"));
        let mut image = fixture(&specs);
        image.layers[0].content = LayerContent::Raster(Surface::new(image.pixel_format()));
        assert!(split(&image, &specs).unwrap_err().contains("empty"));
    }
}
