//! Length-delimited source transfers. Native files never become base64 JSON.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const METADATA_LIMIT: usize = 4 * 1024 * 1024;
pub const TRANSFER_LIMIT: usize = 512 * 1024 * 1024;
const DEPENDENCY_LIMIT: usize = 4096;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WireError(String);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Header {
    metadata: Value,
    native_length: u64,
    dependencies: Vec<Dependency>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependency {
    hash: String,
    length: u64,
}

pub struct Transfer {
    pub metadata: Value,
    pub native: Vec<u8>,
    pub dependencies: BTreeMap<String, Vec<u8>>,
}

pub fn encode(metadata: Value, native: &[u8], dependencies: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, WireError> {
    if dependencies.len() > DEPENDENCY_LIMIT {
        return Err(WireError("Too many creative dependencies".into()));
    }
    let header = Header {
        metadata,
        native_length: native.len() as u64,
        dependencies: dependencies.iter().map(|(hash, data)| Dependency { hash: hash.clone(), length: data.len() as u64 }).collect(),
    };
    let header = serde_json::to_vec(&header).map_err(|e| WireError(e.to_string()))?;
    if header.len() > METADATA_LIMIT {
        return Err(WireError("Source metadata exceeds transfer limit".into()));
    }
    let size = dependencies
        .values()
        .fold(4_usize.checked_add(header.len()).and_then(|n| n.checked_add(native.len())), |n, data| n.and_then(|n| n.checked_add(data.len())))
        .ok_or_else(|| WireError("Source transfer size overflow".into()))?;
    if size > TRANSFER_LIMIT {
        return Err(WireError("Source transfer exceeds configured limit".into()));
    }
    let length = u32::try_from(header.len()).map_err(|_| WireError("Source metadata is too large".into()))?;
    let mut result = Vec::new();
    result.try_reserve_exact(size).map_err(|_| WireError("Not enough memory for source transfer".into()))?;
    result.extend_from_slice(&length.to_be_bytes());
    result.extend_from_slice(&header);
    result.extend_from_slice(native);
    for bytes in dependencies.values() {
        result.extend_from_slice(bytes);
    }
    Ok(result)
}

pub fn decode(bytes: &[u8]) -> Result<Transfer, WireError> {
    if bytes.len() > TRANSFER_LIMIT {
        return Err(WireError("Source transfer exceeds configured limit".into()));
    }
    let prefix: [u8; 4] =
        bytes.get(..4).ok_or_else(|| WireError("Incomplete transfer header".into()))?.try_into().map_err(|_| WireError("Invalid transfer header".into()))?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > METADATA_LIMIT {
        return Err(WireError("Source metadata exceeds transfer limit".into()));
    }
    let mut cursor = 4_usize;
    let header: Header = serde_json::from_slice(take(bytes, &mut cursor, length as u64)?).map_err(|e| WireError(e.to_string()))?;
    if header.dependencies.len() > DEPENDENCY_LIMIT {
        return Err(WireError("Too many creative dependencies".into()));
    }
    let native = take(bytes, &mut cursor, header.native_length)?.to_vec();
    let mut dependencies = BTreeMap::new();
    for entry in header.dependencies {
        if dependencies.contains_key(&entry.hash) {
            return Err(WireError("Duplicate creative dependency".into()));
        }
        dependencies.insert(entry.hash, take(bytes, &mut cursor, entry.length)?.to_vec());
    }
    if cursor != bytes.len() {
        return Err(WireError("Unexpected trailing transfer bytes".into()));
    }
    Ok(Transfer { metadata: header.metadata, native, dependencies })
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, length: u64) -> Result<&'a [u8], WireError> {
    let length = usize::try_from(length).map_err(|_| WireError("Transfer length overflow".into()))?;
    let end = cursor.checked_add(length).ok_or_else(|| WireError("Transfer length overflow".into()))?;
    let result = bytes.get(*cursor..end).ok_or_else(|| WireError("Incomplete source transfer".into()))?;
    *cursor = end;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_binary_roundtrip_preserves_bytes_and_dependencies() {
        let dependencies = BTreeMap::from([("sha256:test".into(), vec![0, 255, 3])]);
        let encoded = encode(json!({"revision":"first"}), b"native\0bytes", &dependencies).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.metadata["revision"], "first");
        assert_eq!(decoded.native, b"native\0bytes");
        assert_eq!(decoded.dependencies, dependencies);
        assert!(!String::from_utf8_lossy(&encoded).contains("bmF0aXZl"));
    }

    #[test]
    fn truncated_overflow_and_extra_transfers_are_rejected() {
        let encoded = encode(json!({}), b"native", &BTreeMap::new()).unwrap();
        for end in 0..encoded.len() {
            assert!(decode(&encoded[..end]).is_err());
        }
        let mut trailing = encoded;
        trailing.push(1);
        assert!(decode(&trailing).is_err());
        assert!(decode(&u32::MAX.to_be_bytes()).is_err());
        let header = serde_json::to_vec(&json!({"metadata":{},"nativeLength":u64::MAX,"dependencies":[]})).unwrap();
        let mut impossible = (header.len() as u32).to_be_bytes().to_vec();
        impossible.extend_from_slice(&header);
        assert!(decode(&impossible).is_err());
    }
}
