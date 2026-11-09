//! Test-only decoder for losslessly interned frozen oracle JSON.
use serde_json::Value;

pub(super) fn decode(value: Value) -> Result<Value, String> {
    if value.is_array() {
        return Ok(value);
    }
    if value["format"] != "jterm-oracle-intern-v1" {
        return Err("unknown oracle format".into());
    }
    let nodes = value["nodes"].as_array().ok_or("missing nodes")?;
    let mut decoded: Vec<Value> = Vec::with_capacity(nodes.len());
    let lookup = |index: &Value, values: &[Value]| -> Result<Value, String> {
        let index = index.as_u64().ok_or("invalid reference")?;
        values
            .get(usize::try_from(index).map_err(|_| "reference overflow")?)
            .cloned()
            .ok_or_else(|| "forward or missing reference".into())
    };
    for node in nodes {
        let node = node
            .as_array()
            .filter(|n| n.len() == 2)
            .ok_or("invalid node")?;
        let payload = &node[1];
        let item = match node[0].as_u64() {
            Some(0) => payload.clone(),
            Some(1) => Value::Array(
                payload
                    .as_array()
                    .ok_or("invalid array")?
                    .iter()
                    .map(|id| lookup(id, &decoded))
                    .collect::<Result<_, _>>()?,
            ),
            Some(tag @ (2 | 3)) => {
                let (mut object, changes) = if tag == 2 {
                    (
                        serde_json::Map::new(),
                        payload.as_array().ok_or("invalid object")?,
                    )
                } else {
                    let patch = payload
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or("invalid patch")?;
                    let base = lookup(&patch[0], &decoded)?;
                    (
                        base.as_object().ok_or("patch base is not object")?.clone(),
                        patch[1].as_array().ok_or("invalid changes")?,
                    )
                };
                for pair in changes {
                    let pair = pair
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or("invalid pair")?;
                    let key = lookup(&pair[0], &decoded)?;
                    object.insert(
                        key.as_str().ok_or("non-string key")?.to_owned(),
                        lookup(&pair[1], &decoded)?,
                    );
                }
                Value::Object(object)
            }
            _ => return Err("unknown node tag".into()),
        };
        decoded.push(item);
    }
    lookup(&value["root"], &decoded)
}

#[test]
fn decodes_arrays_objects_and_delta_without_mutating_base() {
    let packed = serde_json::json!({"format":"jterm-oracle-intern-v1", "root":5,
        "nodes":[[0,"x"],[0,1],[2,[[0,1]]],[0,2],[3,[2,[[0,3]]]],[1,[2,4]]]});
    assert_eq!(
        decode(packed).unwrap(),
        serde_json::json!([{"x":1},{"x":2}])
    );
}
#[test]
fn original_array_is_unchanged() {
    let original = serde_json::json!([["unicode 界", 42, {"key":false}]]);
    assert_eq!(decode(original.clone()).unwrap(), original);
}
#[test]
fn rejects_forward_reference_and_unknown_tag() {
    for nodes in [
        serde_json::json!([[1, [0]]]),
        serde_json::json!([[9, null]]),
    ] {
        assert!(decode(
            serde_json::json!({"format":"jterm-oracle-intern-v1","root":0,"nodes":nodes})
        )
        .is_err());
    }
}
#[test]
fn rejects_non_object_patch_base() {
    assert!(decode(
        serde_json::json!({"format":"jterm-oracle-intern-v1","root":1,
        "nodes":[[0,1],[3,[0,[]]]]})
    )
    .is_err());
}
