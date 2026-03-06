use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepMetadata {
    pub id: Uuid,
    pub label: String,
    pub description: Option<String>,
    pub inputs: Vec<TypeInfo>,
    pub outputs: Vec<TypeInfo>,
}

impl StepMetadata {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeInfo {
    pub name: String,
    /// Optional JSON Schema describing this type's structure.
    /// Populated via `.with_input_schema::<T>()` / `.with_output_schema::<T>()` or `#[transition(schema)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
}

impl TypeInfo {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            json_schema: None,
        }
    }

    pub fn with_json_schema(mut self, schema: serde_json::Value) -> Self {
        self.json_schema = Some(schema);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn type_info_new_has_no_schema() {
        let info = TypeInfo::new("i32");
        assert_eq!(info.name, "i32");
        assert!(info.json_schema.is_none());
    }

    #[test]
    fn type_info_with_json_schema_builder() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "integer"}}});
        let info = TypeInfo::new("MyStruct").with_json_schema(schema.clone());
        assert_eq!(info.json_schema.unwrap(), schema);
    }

    #[test]
    fn type_info_json_schema_omitted_when_none() {
        let info = TypeInfo::new("i32");
        let json = serde_json::to_value(&info).unwrap();
        assert!(!json.as_object().unwrap().contains_key("json_schema"));
    }

    #[test]
    fn type_info_json_schema_present_when_some() {
        let schema = json!({"type": "string"});
        let info = TypeInfo::new("String").with_json_schema(schema.clone());
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["json_schema"], schema);
    }

    #[test]
    fn type_info_deserializes_without_json_schema_field() {
        let json = r#"{"name": "u64"}"#;
        let info: TypeInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, "u64");
        assert!(info.json_schema.is_none());
    }

    #[test]
    fn type_info_roundtrip_with_schema() {
        let schema = json!({"type": "array", "items": {"type": "integer"}});
        let info = TypeInfo::new("Vec<i32>").with_json_schema(schema.clone());
        let serialized = serde_json::to_string(&info).unwrap();
        let deserialized: TypeInfo = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.name, "Vec<i32>");
        assert_eq!(deserialized.json_schema.unwrap(), schema);
    }
}
