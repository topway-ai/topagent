use crate::ToolSpec;

#[derive(Debug, Clone)]
pub struct SkillSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl SkillSchema {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }

    pub fn as_tool_spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

impl From<ToolSpec> for SkillSchema {
    fn from(spec: ToolSpec) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            input_schema: spec.input_schema,
        }
    }
}

impl From<SkillSchema> for ToolSpec {
    fn from(schema: SkillSchema) -> Self {
        Self {
            name: schema.name,
            description: schema.description,
            input_schema: schema.input_schema,
        }
    }
}
