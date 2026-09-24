//! The JSON Schema of every protocol message.
//!
//! One document, checked in at `schema/protocol-v1.schema.json`: a frame
//! from a client is a `CommandEnvelope` and a frame from the engine is a
//! `ServerMessage`. Other tools (MCP tool schemas, the desktop's TypeScript
//! types) are generated from this file, so a test fails when it drifts.

use crate::command::CommandEnvelope;
use crate::message::ServerMessage;
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The schema document as a JSON value.
pub fn protocol_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let client = generator.subschema_for::<CommandEnvelope>();
    let server = generator.subschema_for::<ServerMessage>();
    let definitions: Map<String, Value> = generator.definitions().clone();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Fetchpath protocol",
        "description": "Every frame is one message: a CommandEnvelope from a client, or a ServerMessage from the engine.",
        "x-fetchpath-schema-version": crate::SCHEMA_VERSION,
        "oneOf": [client, server],
        "$defs": definitions,
    })
}

/// The schema as the checked-in file holds it: keys sorted at every level so
/// the text does not depend on map ordering, two-space indentation and a
/// final newline.
pub fn protocol_schema_text() -> String {
    let mut text = serde_json::to_string_pretty(&Sorted(&protocol_schema()))
        .expect("a JSON value always serializes");
    text.push('\n');
    text
}

struct Sorted<'a>(&'a Value);

impl Serialize for Sorted<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(object) => {
                let sorted: BTreeMap<&String, Sorted<'_>> = object
                    .iter()
                    .map(|(key, value)| (key, Sorted(value)))
                    .collect();
                sorted.serialize(serializer)
            }
            Value::Array(items) => {
                let items: Vec<Sorted<'_>> = items.iter().map(Sorted).collect();
                items.serialize(serializer)
            }
            other => other.serialize(serializer),
        }
    }
}
