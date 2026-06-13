//! A ready-to-use **MCP Prompt** catalog for working with memories.
//!
//! A prompt is an instruction template the user can select in their MCP client.
//! This module defines the catalog (name, description, arguments) and renders the
//! message text based on the relevant memory contents. The memory-reading logic is
//! provided by the caller (the server) via closures/data, keeping this module pure.

/// Identities of the prompts known to the server.
pub mod names {
    pub const SUMMARIZE_PROJECT: &str = "summarize-project";
    pub const REVIEW_DECISIONS: &str = "review-decisions";
    pub const ONBOARD: &str = "onboard";
}

/// Metadata for a single prompt, shown in `list_prompts`.
pub struct PromptSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// (argument_name, description, required)
    pub arguments: &'static [(&'static str, &'static str, bool)],
}

/// Catalog of all supported prompts.
pub const CATALOG: &[PromptSpec] = &[
    PromptSpec {
        name: names::SUMMARIZE_PROJECT,
        description: "Summarize all of a project's memories into a brief overview.",
        arguments: &[(
            "project",
            "Project name (optional; auto-detected if empty).",
            false,
        )],
    },
    PromptSpec {
        name: names::REVIEW_DECISIONS,
        description:
            "Review all memories of type 'decision' and assess whether they are still relevant.",
        arguments: &[(
            "project",
            "Project name (optional; auto-detected if empty).",
            false,
        )],
    },
    PromptSpec {
        name: names::ONBOARD,
        description: "Explain this project to a new member based on the existing memories.",
        arguments: &[(
            "project",
            "Project name (optional; auto-detected if empty).",
            false,
        )],
    },
];

/// A summary of a single memory to render into the prompt text.
pub struct MemoryBrief {
    pub name: String,
    pub kind: String,
    pub description: String,
}

/// Render the message body of the `summarize-project` prompt.
pub fn render_summarize(project: &str, mems: &[MemoryBrief]) -> String {
    let mut s = format!(
        "You are reviewing the memory base of the project \"{project}\". \
         Here is the list of memories (title — type — description):\n\n"
    );
    append_list(&mut s, mems);
    s.push_str(
        "\nTask: write a brief overview (3–6 sentences) describing this project, \
         its main themes, and the important decisions that have been made. End with \
         a list of 'things that still need clarification' if any.",
    );
    s
}

/// Render the message body of the `review-decisions` prompt.
pub fn render_review_decisions(project: &str, decisions: &[MemoryBrief]) -> String {
    if decisions.is_empty() {
        return format!(
            "Project \"{project}\" has no memories of type 'decision'. \
             Suggest which decisions should be documented \
             based on the context you are aware of."
        );
    }
    let mut s = format!("Review the decisions (type=decision) in the project \"{project}\":\n\n");
    append_list(&mut s, decisions);
    s.push_str(
        "\nFor each decision: assess whether it is (a) still relevant, (b) needs to be reviewed again, \
         or (c) is now obsolete. Give a brief rationale and a follow-up recommendation.",
    );
    s
}

/// Render the message body of the `onboard` prompt.
pub fn render_onboard(project: &str, mems: &[MemoryBrief]) -> String {
    let mut s = format!(
        "A new member is joining the project \"{project}\". \
         Here are the available memories:\n\n"
    );
    append_list(&mut s, mems);
    s.push_str(
        "\nTask: write a beginner-friendly onboarding explanation — start with the big picture, \
         then the important components, then where they can start contributing. \
         Reference related memories by mentioning their titles.",
    );
    s
}

fn append_list(s: &mut String, mems: &[MemoryBrief]) {
    if mems.is_empty() {
        s.push_str("_(no memories yet)_\n");
        return;
    }
    for m in mems {
        s.push_str(&format!("- {} — [{}] {}\n", m.name, m.kind, m.description));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn briefs() -> Vec<MemoryBrief> {
        vec![
            MemoryBrief {
                name: "auth-flow".into(),
                kind: "project".into(),
                description: "authentication".into(),
            },
            MemoryBrief {
                name: "pakai-rust".into(),
                kind: "decision".into(),
                description: "why rust".into(),
            },
        ]
    }

    #[test]
    fn catalog_has_three_prompts() {
        assert_eq!(CATALOG.len(), 3);
        assert!(CATALOG.iter().any(|p| p.name == names::SUMMARIZE_PROJECT));
    }

    #[test]
    fn summarize_lists_memories() {
        let out = render_summarize("demo", &briefs());
        assert!(out.contains("demo"));
        assert!(out.contains("auth-flow"));
        assert!(out.contains("[decision]"));
    }

    #[test]
    fn review_decisions_handles_empty() {
        let out = render_review_decisions("demo", &[]);
        assert!(out.contains("has no memories of type 'decision'"));
    }
}
