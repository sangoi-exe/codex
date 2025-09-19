use std::collections::HashSet;

use serde_json::json;

use mcp_types::Tool;
use mcp_types::ToolInputSchema;

use crate::McpServerOpts;
use crate::codex_tool_config::create_tool_for_codex_tool_call_param;
use crate::codex_tool_config::create_tool_for_codex_tool_call_reply_param;

const ACTION_TOOLS: &[(&str, &str)] = &[
    ("codex.newConversation", "Start a new Codex conversation."),
    (
        "codex.listConversations",
        "List recorded Codex conversations.",
    ),
    (
        "codex.resumeConversation",
        "Resume a recorded Codex conversation from a rollout file.",
    ),
    (
        "codex.archiveConversation",
        "Archive a Codex conversation and move its rollout into the archived directory.",
    ),
    (
        "codex.sendUserMessage",
        "Send user message input to an active Codex conversation.",
    ),
    (
        "codex.sendUserTurn",
        "Submit a full user turn (message plus overrides) to an active Codex conversation.",
    ),
    (
        "codex.interruptConversation",
        "Request that Codex interrupt the active task for a conversation.",
    ),
    (
        "codex.gitDiffToRemote",
        "Return the diff between the working tree and its remote for a given directory.",
    ),
    (
        "codex.loginApiKey",
        "Store an API key for Codex to use when authenticating with OpenAI services.",
    ),
    (
        "codex.loginChatGpt",
        "Initiate ChatGPT OAuth login and return the authorization URL.",
    ),
    (
        "codex.cancelLoginChatGpt",
        "Cancel an in-flight ChatGPT OAuth login request.",
    ),
    ("codex.logoutChatGpt", "Clear stored ChatGPT OAuth tokens."),
    (
        "codex.getAuthStatus",
        "Return Codex authentication status for the active user.",
    ),
    (
        "codex.getUserSavedConfig",
        "Return the saved Codex configuration from config.toml.",
    ),
    (
        "codex.setDefaultModel",
        "Persist the default model (and optionally reasoning effort) in the user's config.",
    ),
    ("codex.getUserAgent", "Return the Codex user agent string."),
    (
        "codex.userInfo",
        "Return best-effort user identity information inferred from stored credentials.",
    ),
    (
        "codex.execCommand",
        "Execute a one-off shell command using Codex's sandbox policy.",
    ),
];

const AUX_TOOLS: &[(&str, &str)] = &[
    (
        "codex.spawnAuxAgent",
        "Spawn an auxiliary Codex CLI instance with a prompt.",
    ),
    (
        "codex.stopAuxAgent",
        "Terminate a running auxiliary Codex CLI instance.",
    ),
    (
        "codex.listAuxAgents",
        "List active auxiliary Codex CLI instances.",
    ),
];

pub fn compute_tool_names(opts: &McpServerOpts, max_aux_agents: Option<usize>) -> Vec<String> {
    let mut ordered = Vec::new();
    ordered.push("reply".to_string());

    if opts.enable_foo {
        ordered.push("foo".to_string());
    }

    if opts.expose_all_tools {
        ordered.push("codex".to_string());
        ordered.push("codex-reply".to_string());

        for (name, _) in ACTION_TOOLS {
            ordered.push((*name).to_string());
        }

        if max_aux_agents.unwrap_or(0) > 0 {
            for (name, _) in AUX_TOOLS {
                ordered.push((*name).to_string());
            }
        }
    }

    dedupe_preserving_order(ordered)
}

pub fn list_tools(
    opts: &McpServerOpts,
    max_aux_agents: Option<usize>,
) -> Result<Vec<Tool>, String> {
    let names = compute_tool_names(opts, max_aux_agents);
    let mut tools = Vec::with_capacity(names.len());
    let mut seen = HashSet::new();

    for name in names {
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate tool '{name}'"));
        }

        let tool = build_tool_by_name(&name, max_aux_agents)
            .ok_or_else(|| format!("unknown tool '{name}'"))?;
        validate_tool_schema(&tool)?;
        tools.push(tool);
    }

    Ok(tools)
}

fn dedupe_preserving_order(mut names: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

fn build_tool_by_name(name: &str, max_aux_agents: Option<usize>) -> Option<Tool> {
    match name {
        "reply" => Some(create_reply_tool()),
        "foo" => Some(create_foo_tool()),
        "codex" => Some(create_tool_for_codex_tool_call_param()),
        "codex-reply" => Some(create_tool_for_codex_tool_call_reply_param()),
        other => full_action_tool_by_name(other, max_aux_agents),
    }
}

fn full_action_tool_by_name(name: &str, max_aux_agents: Option<usize>) -> Option<Tool> {
    for (tool_name, description) in ACTION_TOOLS {
        if *tool_name == name {
            return Some(build_simple_tool(tool_name, description));
        }
    }

    if max_aux_agents.unwrap_or(0) > 0 {
        for (tool_name, description) in AUX_TOOLS {
            if *tool_name == name {
                return Some(build_simple_tool(tool_name, description));
            }
        }
    }

    None
}

fn create_reply_tool() -> Tool {
    let properties = json!({
        "prompt": {
            "type": "string",
            "description": "User message forwarded to Codex for a quick reply.",
        }
    });

    Tool {
        name: "reply".to_string(),
        title: Some("Reply".to_string()),
        description: Some("Send a single prompt to Codex using default settings.".to_string()),
        input_schema: ToolInputSchema {
            properties: Some(properties),
            required: Some(vec!["prompt".to_string()]),
            r#type: "object".to_string(),
        },
        output_schema: None,
        annotations: None,
    }
}

fn create_foo_tool() -> Tool {
    let properties = json!({
        "message": {
            "type": "string",
            "description": "Optional text echoed back for diagnostics.",
        }
    });

    Tool {
        name: "foo".to_string(),
        title: Some("Foo".to_string()),
        description: Some(
            "Internal diagnostics tool exposed when --enable-foo is set.".to_string(),
        ),
        input_schema: ToolInputSchema {
            properties: Some(properties),
            required: None,
            r#type: "object".to_string(),
        },
        output_schema: None,
        annotations: None,
    }
}

fn build_simple_tool(name: &str, description: &str) -> Tool {
    Tool {
        name: name.to_string(),
        title: Some(name.to_string()),
        description: Some(description.to_string()),
        input_schema: ToolInputSchema {
            properties: None,
            required: None,
            r#type: "object".to_string(),
        },
        output_schema: None,
        annotations: None,
    }
}

fn validate_tool_schema(tool: &Tool) -> Result<(), String> {
    if tool.name.trim().is_empty() {
        return Err("tool name cannot be empty".to_string());
    }
    if tool.input_schema.r#type.trim().is_empty() {
        return Err(format!(
            "tool '{}' is missing an input schema type",
            tool.name
        ));
    }

    serde_json::to_value(tool)
        .map(|_| ())
        .map_err(|err| format!("tool '{}' schema failed to serialize: {err}", tool.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn empty_opts() -> McpServerOpts {
        McpServerOpts {
            expose_all_tools: false,
            enable_foo: false,
            overrides: Default::default(),
        }
    }

    #[test]
    fn default_compute_tool_names_returns_reply_only() {
        let names = compute_tool_names(&empty_opts(), None);
        assert_eq!(names, vec!["reply".to_string()]);
    }

    #[test]
    fn enable_foo_adds_flagged_tool() {
        let mut opts = empty_opts();
        opts.enable_foo = true;
        let names = compute_tool_names(&opts, None);
        assert_eq!(names, vec!["reply".to_string(), "foo".to_string()]);
    }

    #[test]
    fn expose_all_tools_includes_core_and_aux() {
        let mut opts = empty_opts();
        opts.expose_all_tools = true;
        let names = compute_tool_names(&opts, Some(2));
        assert!(names.contains(&"codex".to_string()));
        assert!(names.contains(&"codex.execCommand".to_string()));
        assert!(names.contains(&"codex.spawnAuxAgent".to_string()));
        assert!(names.contains(&"reply".to_string()));
        assert!(!names.contains(&"foo".to_string()));
    }

    #[test]
    fn list_tools_validates_and_returns_structures() {
        let mut opts = empty_opts();
        opts.enable_foo = true;
        let tools = list_tools(&opts, None).expect("list tools");
        let names: Vec<_> = tools.iter().map(|tool| tool.name.clone()).collect();
        assert_eq!(names, vec!["reply".to_string(), "foo".to_string()]);
    }
}
