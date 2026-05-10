use crate::tool_spec::ToolSpec;

pub const MAX_RUN_CHECKPOINT_CHARS: usize = 2_000;
pub const MAX_DEFAULT_PROVIDER_TOOL_COUNT: usize = 24;
pub const MAX_DEFAULT_PROVIDER_TOOL_SCHEMA_CHARS: usize = 24_000;
pub const MAX_MEMORY_BRIEFING_CHARS: usize = 6_000;
pub const MAX_TRANSCRIPT_SNIPPET_CHARS: usize = 1_500;
pub const MAX_PROCEDURE_SNIPPET_CHARS: usize = 1_200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptBudgetUsage {
    pub rendered_system_prompt_chars: usize,
    pub memory_briefing_chars: usize,
    pub procedure_snippet_chars: usize,
    pub transcript_snippet_chars: usize,
    pub checkpoint_chars: usize,
    pub provider_tool_count: usize,
    pub serialized_provider_tool_schema_chars: usize,
}

impl PromptBudgetUsage {
    pub fn from_prompt_parts(
        rendered_system_prompt: &str,
        memory_briefing: Option<&str>,
        checkpoint: Option<&str>,
        provider_tools: &[ToolSpec],
    ) -> Self {
        let memory = memory_briefing.unwrap_or_default();
        Self {
            rendered_system_prompt_chars: rendered_system_prompt.chars().count(),
            memory_briefing_chars: memory.chars().count(),
            procedure_snippet_chars: section_chars(memory, "### Relevant Procedures"),
            transcript_snippet_chars: section_chars(memory, "### Transcript Evidence"),
            checkpoint_chars: checkpoint.unwrap_or_default().chars().count(),
            provider_tool_count: provider_tools.len(),
            serialized_provider_tool_schema_chars: serialized_provider_tool_schema_chars(
                provider_tools,
            ),
        }
    }

    pub fn validate_default_mode(&self) -> Result<(), String> {
        if self.checkpoint_chars > MAX_RUN_CHECKPOINT_CHARS {
            return Err(format!(
                "RunCheckpoint is {} chars, above {}. Inspect checkpoint rendering, receipt anchors, and workflow gaps before raising the threshold.",
                self.checkpoint_chars, MAX_RUN_CHECKPOINT_CHARS
            ));
        }
        if self.memory_briefing_chars > MAX_MEMORY_BRIEFING_CHARS {
            return Err(format!(
                "memory briefing is {} chars, above {}. Inspect memory/procedure/transcript retrieval caps before raising the threshold.",
                self.memory_briefing_chars, MAX_MEMORY_BRIEFING_CHARS
            ));
        }
        if self.transcript_snippet_chars > MAX_TRANSCRIPT_SNIPPET_CHARS {
            return Err(format!(
                "transcript snippet section is {} chars, above {}. Inspect transcript retrieval caps before raising the threshold.",
                self.transcript_snippet_chars, MAX_TRANSCRIPT_SNIPPET_CHARS
            ));
        }
        if self.procedure_snippet_chars > MAX_PROCEDURE_SNIPPET_CHARS {
            return Err(format!(
                "procedure snippet section is {} chars, above {}. Inspect procedure retrieval caps before raising the threshold.",
                self.procedure_snippet_chars, MAX_PROCEDURE_SNIPPET_CHARS
            ));
        }
        validate_provider_tool_budget(
            self.provider_tool_count,
            self.serialized_provider_tool_schema_chars,
        )
    }
}

pub fn serialized_provider_tool_schema_chars(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .map(|spec| spec.name.len() + spec.description.len() + spec.input_schema.to_string().len())
        .sum()
}

pub fn validate_provider_tool_budget(tool_count: usize, schema_chars: usize) -> Result<(), String> {
    if tool_count > MAX_DEFAULT_PROVIDER_TOOL_COUNT {
        return Err(format!(
            "default provider tool count grew to {tool_count}, above {MAX_DEFAULT_PROVIDER_TOOL_COUNT}. Inspect Skill registration and phase exposure; tool budget increases need a REVIEW_RULES.md rationale."
        ));
    }
    if schema_chars > MAX_DEFAULT_PROVIDER_TOOL_SCHEMA_CHARS {
        return Err(format!(
            "default provider tool schema grew to {schema_chars} chars, above {MAX_DEFAULT_PROVIDER_TOOL_SCHEMA_CHARS}. Inspect ToolSpec descriptions/schemas and Skill registration; schema budget increases need a REVIEW_RULES.md rationale."
        ));
    }
    Ok(())
}

fn section_chars(text: &str, heading: &str) -> usize {
    let Some(start) = text.find(heading) else {
        return 0;
    };
    let tail = &text[start..];
    let end = tail
        .find("\n### ")
        .filter(|end| *end > 0)
        .unwrap_or(tail.len());
    tail[..end].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_budget_error_explains_what_to_inspect() {
        let err = validate_provider_tool_budget(MAX_DEFAULT_PROVIDER_TOOL_COUNT + 1, 1)
            .expect_err("budget should fail");

        assert!(err.contains("Inspect Skill registration"));
        assert!(err.contains("REVIEW_RULES.md"));
    }

    #[test]
    fn prompt_budget_tracks_transcript_and_procedure_sections() {
        let memory = "### Relevant Procedures\nshort\n### Transcript Evidence\nprior chat";
        let usage = PromptBudgetUsage::from_prompt_parts(
            "system",
            Some(memory),
            Some("checkpoint"),
            &[ToolSpec::read()],
        );

        assert!(usage.procedure_snippet_chars > 0);
        assert!(usage.transcript_snippet_chars > 0);
        assert_eq!(usage.checkpoint_chars, "checkpoint".len());
    }
}
