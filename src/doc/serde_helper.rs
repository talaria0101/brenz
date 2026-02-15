//! Deserialize with lowercase keys
//!
//! Thanks to ChatGPT for guidance

use serde::{Deserialize, Deserializer};
use serde::de::DeserializeOwned;
use std::collections::HashMap;

/*
/// Generic serializer: emit map with lowercase keys (deterministic order via BTreeMap).
pub(crate) fn serialize_lower_map<S, V>(
    map: &HashMap<String, V>,
    serializer: S
) -> Result<S::Ok, S::Error>
where S: Serializer, V: Serialize
{
    let mut lowered: BTreeMap<String, &V> = BTreeMap::new();
    for (k, v) in map.iter() {
        lowered.insert(k.to_ascii_lowercase(), v);
    }

    lowered.serialize(serializer)
}
*/

pub(crate) fn deserialize_lower_map<'de, D, V>(
    deserializer: D
) -> Result<HashMap<String, V>, D::Error>
where D: Deserializer<'de>, V: DeserializeOwned
{
    let raw = HashMap::<String, V>::deserialize(deserializer)?;
    let mut lowered = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        lowered.insert(k.to_ascii_lowercase(), v);
    }

    Ok(lowered)
}
