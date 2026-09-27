//! The author's door (SCHEMA.md §10): repair what has one reading, then
//! validate. A repair rewrites the author's JSON only at its own path, and a
//! document that needed none comes back as the author's own text.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::schema::{self, SchemaError};
use crate::validate::{self, Compiled, HOST_ENVELOPES};

pub struct Admission {
    pub document: String,
    pub repairs: Vec<Repair>,
    pub compiled: Compiled,
}

pub struct Repair {
    pub path: String,
    pub rule: &'static str,
    pub message: String,
}

impl Repair {
    fn new(path: String, rule: &'static str, detail: String) -> Self {
        let message = format!("{path}: {detail}");
        Repair { path, rule, message }
    }
}

/// The standard signals a host drives (§2), with the type a document that
/// reads one without declaring it is given.
const HOST_SIGNALS: [(&str, &str); 6] = [
    ("inProgress", "unit"),
    ("outProgress", "unit"),
    ("time", "time"),
    ("activations", "unitArray"),
    ("emphasis", "unitArray"),
    ("focus", "unitArray"),
];

pub fn admit(json: &str) -> Result<Admission, SchemaError> {
    schema::parse(json)?;
    let mut document: Value =
        serde_json::from_str(json).map_err(|e| SchemaError::new("", e.to_string()))?;
    let mut repairs = Vec::new();
    hold_track_ends(&mut document, &mut repairs);
    settle_envelopes(&mut document, &mut repairs);
    declare_host_signals(&mut document, &mut repairs);
    let document = if repairs.is_empty() {
        json.to_owned()
    } else {
        document.to_string()
    };
    let compiled = validate::validate(schema::parse(&document)?)?;
    Ok(Admission {
        document,
        repairs,
        compiled,
    })
}

/// A track that starts after 0 or ends before 1 holds its end values there.
/// Only a track with one reading: two or more keys, inside [0, 1], strictly
/// increasing, and no `ease` on its first key (arriving or leaving?).
fn hold_track_ends(document: &mut Value, repairs: &mut Vec<Repair>) {
    let Some(animators) = document.get_mut("animators").and_then(Value::as_array_mut) else {
        return;
    };
    for (index, animator) in animators.iter_mut().enumerate() {
        let Some(keys) = animator.get_mut("keyframes").and_then(Value::as_array_mut) else {
            continue;
        };
        let Some(positions) = keys
            .iter()
            .map(|key| key.get("at").and_then(Value::as_f64))
            .collect::<Option<Vec<f64>>>()
        else {
            continue;
        };
        let one_reading = positions.len() >= 2
            && positions.iter().all(|at| (0.0..=1.0).contains(at))
            && positions.windows(2).all(|pair| pair[0] < pair[1])
            && keys[0].get("ease").is_none();
        if !one_reading {
            continue;
        }
        let (first, last) = (positions[0], positions[positions.len() - 1]);
        let detail = match (first > 0.0, last < 1.0) {
            (true, true) => {
                "added {\"at\":0} holding its first value and {\"at\":1} holding its last"
            }
            (true, false) => "added {\"at\":0} holding its first value",
            (false, true) => "added {\"at\":1} holding its last value",
            (false, false) => continue,
        };
        if first > 0.0 {
            let value = keys[0]["value"].clone();
            keys.insert(0, json!({ "at": 0, "value": value }));
        }
        if last < 1.0 {
            let value = keys[keys.len() - 1]["value"].clone();
            keys.push(json!({ "at": 1, "value": value }));
        }
        repairs.push(Repair::new(
            format!("animators[{index}].keyframes"),
            "keyframes-span",
            format!("the track ran {first}–{last}; {detail}"),
        ));
    }
}

/// An envelope declared away from its settled value rests there (§2).
fn settle_envelopes(document: &mut Value, repairs: &mut Vec<Repair>) {
    let Some(inputs) = document.get_mut("inputs").and_then(Value::as_array_mut) else {
        return;
    };
    for (index, input) in inputs.iter_mut().enumerate() {
        let Some(key) = input.get("key").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let Some(&(_, settled)) = HOST_ENVELOPES.iter().find(|(envelope, _)| *envelope == key)
        else {
            continue;
        };
        if input.get("type").and_then(Value::as_str) != Some("unit") {
            continue;
        }
        let Some(default) = input.get("default").and_then(Value::as_f64) else {
            continue;
        };
        if default == settled {
            continue;
        }
        input["default"] = json!(settled as u8);
        let plays = if key == "inProgress" { "an entrance" } else { "an exit" };
        repairs.push(Repair::new(
            format!("inputs[{index}].default"),
            "settled-envelope",
            format!(
                "{key} rests settled at {settled} (was {default}) — the host drives it only while \
                 {plays} plays"
            ),
        ));
    }
}

/// A document that reads a standard signal it never declared is given the
/// declaration the host drives (§2).
fn declare_host_signals(document: &mut Value, repairs: &mut Vec<Repair>) {
    let declared: HashSet<String> = document
        .get("inputs")
        .and_then(Value::as_array)
        .map(|inputs| {
            inputs
                .iter()
                .filter_map(|input| input.get("key").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut reads = Vec::new();
    if let Value::Object(fields) = &*document {
        for (field, value) in fields {
            if field != "inputs" {
                collect_reads(value, field, &mut reads);
            }
        }
    }

    let mut given = HashSet::new();
    let mut declarations = Vec::new();
    for (signal, path) in reads {
        let Some(&(_, kind)) = HOST_SIGNALS.iter().find(|(host, _)| *host == signal) else {
            continue;
        };
        if declared.contains(&signal) || !given.insert(signal.clone()) {
            continue;
        }
        let default = match HOST_ENVELOPES.iter().find(|(envelope, _)| *envelope == signal) {
            Some(&(_, settled)) => json!(settled as u8),
            None if kind == "time" => json!(0),
            None => json!([]),
        };
        repairs.push(Repair::new(
            "inputs".to_owned(),
            "host-signal",
            format!("declared \"{signal}\" ({kind}, default {default}) — {path} reads it"),
        ));
        declarations.push(json!({ "key": signal, "type": kind, "default": default }));
    }
    if declarations.is_empty() {
        return;
    }
    let fields = document.as_object_mut().expect("a parsed document is an object");
    fields
        .entry("inputs")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("a parsed document's inputs are a list")
        .extend(declarations);
}

/// Every input the document reads, in document order: `{ "input": key }`
/// bindings and `driver` names.
fn collect_reads(value: &Value, path: &str, reads: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) => {
            if let (1, Some(Value::String(key))) = (fields.len(), fields.get("input")) {
                reads.push((key.clone(), path.to_owned()));
                return;
            }
            for (field, nested) in fields {
                let nested_path = format!("{path}.{field}");
                match (field.as_str(), nested) {
                    ("driver", Value::String(signal)) => reads.push((signal.clone(), nested_path)),
                    _ => collect_reads(nested, &nested_path, reads),
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_reads(item, &format!("{path}[{index}]"), reads);
            }
        }
        _ => {}
    }
}
