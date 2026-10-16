//! Pin linked smart-object inputs into portable source packages, outside runtime assets.
use crate::defold_project_model::*;
use photocraft_doc::Document;
#[cfg(not(target_arch = "wasm32"))]
use photocraft_doc::{LayerContent, SmartSource};
use std::collections::BTreeMap;

#[cfg(not(target_arch = "wasm32"))]
const PREFIX: &str = "@photocraft-source/";

pub(crate) struct Capture {
    pub native: Vec<u8>,
    pub identity_map: BTreeMap<u64, StableLayerId>,
    pub dependencies: Vec<SourceDependency>,
    pub bodies: BTreeMap<ContentHash, Vec<u8>>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn capture(document: &Document) -> ProjectResult<Capture> {
    use crate::defold_project_fs as disk;
    use std::path::{Path, PathBuf};
    let design = document.defold.as_ref().ok_or_else(|| fail("Linked GUI identity is missing"))?;
    let project = design.project.as_ref().ok_or_else(|| fail("Stable creative source identities are missing"))?;
    let root = std::fs::canonicalize(&design.root).map_err(io)?;
    let mut portable = document.clone();
    crate::defold_shared_design::strip_shared_caches(&mut portable).map_err(io)?;
    let pinned_inputs = portable.clone();
    let mut dependencies = Vec::new();
    let mut bodies = BTreeMap::new();
    let mut total = 0_u64;
    for (_, _, layer) in pinned_inputs.walk() {
        let LayerContent::Smart(object) = &layer.content else { continue };
        let SmartSource::Linked { path } = &object.source else { continue };
        let candidate = Path::new(path);
        let candidate: PathBuf = if candidate.is_absolute() { candidate.into() } else { root.join(candidate) };
        // Saving an imported document must not read arbitrary paths embedded in it.
        let candidate = std::fs::canonicalize(candidate).map_err(io)?;
        let relative =
            candidate.strip_prefix(&root).map_err(|_| fail("A linked smart object is outside this project; embed it before sharing the creative source"))?;
        let scoped = disk::scoped_path(&root, &relative.to_string_lossy().replace('\\', "/"), false)?;
        let bytes = disk::read_bounded(&scoped, ProjectLimits::default().max_dependency_bytes)?;
        total = total.checked_add(bytes.len() as u64).ok_or_else(|| fail("Creative source dependency size overflow"))?;
        if total > (crate::defold_project_wire::TRANSFER_LIMIT - crate::defold_project_wire::METADATA_LIMIT) as u64 {
            return Err(fail("Linked creative sources exceed transfer limit"));
        }
        let identity = project.layers.get(&layer.id.0).ok_or_else(|| fail("Linked smart object has no stable layer identity"))?;
        let extension =
            candidate.extension().and_then(|v| v.to_str()).filter(|v| v.len() <= 12 && v.bytes().all(|c| c.is_ascii_alphanumeric())).unwrap_or("bin");
        let logical_path = format!("linked/{}/source.{extension}", identity.layer_id);
        let content = BlobDescriptor::of_bytes(&bytes);
        bodies.entry(content.hash.clone()).or_insert(bytes);
        dependencies.push(SourceDependency { dependency_id: format!("smart:{}", identity.layer_id), logical_path: logical_path.clone(), content });
        let target = portable.layer_mut(layer.id).ok_or_else(|| fail("Linked smart object disappeared during capture"))?;
        if let LayerContent::Smart(object) = &mut target.content {
            object.source = SmartSource::Linked { path: format!("{PREFIX}{logical_path}") };
        }
    }
    let native = photocraft_format::save_to_bytes(&portable, &Default::default()).map_err(io)?;
    let identity_map = portable
        .defold
        .as_ref()
        .and_then(|design| design.project.as_ref())
        .ok_or_else(|| fail("Portable source identity disappeared"))?
        .layers
        .iter()
        .map(|(local, binding)| (*local, binding.layer_id.clone()))
        .collect();
    Ok(Capture { native, identity_map, dependencies, bodies })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn capture(_: &Document) -> ProjectResult<Capture> {
    Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "Project source storage requires the desktop application"))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn restore(document: &mut Document, source: &SourcePackage, root: &str) -> ProjectResult<()> {
    use crate::defold_project_fs as disk;
    let mut resolved = BTreeMap::new();
    for dependency in &source.envelope.dependencies {
        let bytes = source.payload.dependencies.get(&dependency.content.hash).ok_or_else(|| fail("Pinned creative source dependency is missing"))?;
        dependency.content.verify(bytes)?;
        let relative = format!(
            ".photocraft/local/source-cache/{}/{}/{}",
            source.envelope.package_id,
            dependency.content.hash.as_str().replace(':', "-"),
            dependency.logical_path
        );
        let path = disk::scoped_path(std::path::Path::new(root), &relative, true)?;
        let existing = disk::read_bounded(&path, dependency.content.length).ok();
        if !existing.as_ref().is_some_and(|old| dependency.content.hash.verify(old)) {
            disk::write_atomic(&path, bytes)?;
        }
        resolved.insert(format!("{PREFIX}{}", dependency.logical_path), path.to_string_lossy().into_owned());
    }
    let ids: Vec<_> = document.walk().into_iter().filter_map(|(_, _, layer)| matches!(&layer.content, LayerContent::Smart(_)).then_some(layer.id)).collect();
    for id in ids {
        if let Some(layer) = document.layer_mut(id)
            && let LayerContent::Smart(object) = &mut layer.content
            && let SmartSource::Linked { path } = &mut object.source
        {
            if let Some(resolved) = resolved.get(path) {
                path.clone_from(resolved);
            } else if path.starts_with(PREFIX) {
                return Err(fail("Creative source refers to an unpinned linked object"));
            }
        }
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn restore(_: &mut Document, _: &SourcePackage, _: &str) -> ProjectResult<()> {
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn fail(message: impl Into<String>) -> ProjectError {
    ProjectError::new(ProjectErrorCode::ResourceMissing, message)
}
#[cfg(not(target_arch = "wasm32"))]
fn io(error: impl std::fmt::Display) -> ProjectError {
    ProjectError::new(ProjectErrorCode::Io, error.to_string())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;

    #[test]
    fn linked_inputs_roundtrip_as_pinned_portable_sources_without_touching_runtime() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("game.project"), "[display]\nwidth=32\nheight=32\n").unwrap();
        std::fs::write(dir.path().join("main.gui"), "nodes { id:\"icon\" type:TYPE_BOX size { x:16 y:16 } }").unwrap();
        let source = dir.path().join("art.png");
        let bytes = b"pinned linked smart object fixture";
        std::fs::write(&source, bytes).unwrap();
        let gui = std::fs::read(dir.path().join("main.gui")).unwrap();
        let mut session = Session::new();
        session.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
        let artwork = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].artwork;
        session
            .edit("Link fixture source", |doc, _| {
                doc.layer_mut(photocraft_doc::LayerId(artwork)).unwrap().content = LayerContent::Smart(photocraft_doc::SmartObject::new(
                    SmartSource::Linked { path: source.to_string_lossy().into_owned() },
                    photocraft_geom::Affine::IDENTITY,
                    None,
                ));
                Ok(())
            })
            .unwrap();
        session.execute("defold.saveDesignSource", json!({})).unwrap();
        let doc =
            crate::defold_project_client::load_saved_document(&dir.path().to_string_lossy(), &dir.path().join("main.gui").to_string_lossy()).unwrap().unwrap();
        let artwork = doc.defold.as_ref().unwrap().nodes[0].artwork;
        let LayerContent::Smart(object) = &doc.layer(photocraft_doc::LayerId(artwork)).unwrap().content else { panic!("expected smart source") };
        let SmartSource::Linked { path } = &object.source else { panic!("expected editable linked source") };
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(path.contains("source-cache"));
        // Subsequent saves recapture the verified cache, without an original machine's file.
        std::fs::remove_file(source).unwrap();
        let second = capture(&doc).unwrap();
        assert_eq!(second.dependencies.len(), 1);
        assert_eq!(second.bodies.values().next().unwrap(), bytes);
        assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), gui);
        assert!(!dir.path().join("photocraft_generated").exists());
    }
}
