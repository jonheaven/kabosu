use std::collections::HashMap;

use serde_json::{Map, Value};

lazy_static! {
    pub static ref MANIFEST: Value = serde_json::from_str(include_str!("../../../manifest.json")).expect("valid manifest.json");

    static ref SHORT_KEY_MAPS: HashMap<String, HashMap<String, String>> = {
    let mut out = HashMap::new();
    let Some(root) = MANIFEST.as_object() else {
        return out;
    };

    for (protocol, mapping) in root {
        let Some(entries) = mapping.as_object() else {
            continue;
        };
        let mut protocol_map = HashMap::new();
        for (short, long) in entries {
            if let Some(long_key) = long.as_str() {
                protocol_map.insert(short.clone(), long_key.to_string());
            }
        }
        out.insert(protocol.clone(), protocol_map);
    }

    out
    };
}

pub fn init_manifest() {
    let _ = &*MANIFEST;
    let _ = &*SHORT_KEY_MAPS;
}

pub fn expand_json_keys(body: &[u8]) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object_mut()?;
    let protocol = object
        .get("protocol")
        .and_then(Value::as_str)
        .or_else(|| object.get("p").and_then(Value::as_str))?;

    let mapping = SHORT_KEY_MAPS.get(protocol)?;
    if mapping.is_empty() {
        return Some(body.to_vec());
    }

    let mut expanded = Map::with_capacity(object.len());
    for (key, val) in object.iter() {
        let expanded_key = mapping.get(key).map_or(key.as_str(), String::as_str);
        expanded.insert(expanded_key.to_string(), val.clone());
    }

    *object = expanded;
    serde_json::to_vec(&value).ok()
}
