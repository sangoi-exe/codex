use std::collections::HashSet;

use serde_json::json;
use tracing::debug;

use mcp_types::Tool;
use mcp_types::ToolInputSchema;

use crate::McpServerOpts;
use crate::codex_tool_config::create_tool_for_codex_tool_call_param;
use crate::codex_tool_config::create_tool_for_codex_tool_call_reply_param;

const CODE_EDITING_TOOL_NAMES: &[&str] = &[
    "reply",
    "codex",
    "codex-reply",
    "codex.newConversation",
    "codex.sendUserMessage",
    "codex.sendUserTurn",
    "codex.execCommand",
    "codex.gitDiffToRemote",
];

const CODE_EDITING_ACTIONS: &[(&str, &str)] = &[
    ("codex.newConversation", "Start a new Codex conversation."),
    (
        "codex.sendUserMessage",
        "Send user message input to an active Codex conversation.",
    ),
    (
        "codex.sendUserTurn",
        "Submit a full user turn (message plus overrides) to an active Codex conversation.",
    ),
    (
        "codex.execCommand",
        "Execute a one-off shell command using Codex's sandbox policy.",
    ),
    (
        "codex.gitDiffToRemote",
        "Return the diff between the working tree and its remote for a given directory.",
    ),
];

const ADMIN_ACTIONS: &[(&str, &str)] = &[
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
        "codex.interruptConversation",
        "Request that Codex interrupt the active task for a conversation.",
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
    let mut ordered: Vec<String> = CODE_EDITING_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    if opts.expose_all_tools {
        for (name, _) in ADMIN_ACTIONS {
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

    for name in names.iter() {
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate tool '{name}'"));
        }

        let tool = build_tool_by_name(name, max_aux_agents)
            .ok_or_else(|| format!("unknown tool '{name}'"))?;
        validate_tool_schema(&tool)?;
        tools.push(tool);
    }

    debug!(
        expose_all_tools = opts.expose_all_tools,
        max_aux_agents,
        tools = ?names,
        "announcing MCP tools"
    );

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
        "codex" => Some(create_tool_for_codex_tool_call_param()),
        "codex-reply" => Some(create_tool_for_codex_tool_call_reply_param()),
        other => lookup_action_tool(other)
            .or_else(|| lookup_admin_tool(other))
            .or_else(|| lookup_aux_tool(other, max_aux_agents)),
    }
}

fn lookup_action_tool(name: &str) -> Option<Tool> {
    CODE_EDITING_ACTIONS
        .iter()
        .find(|(tool_name, _)| *tool_name == name)
        .map(|(tool_name, description)| build_simple_tool(tool_name, description))
}

fn lookup_admin_tool(name: &str) -> Option<Tool> {
    ADMIN_ACTIONS
        .iter()
        .find(|(tool_name, _)| *tool_name == name)
        .map(|(tool_name, description)| build_simple_tool(tool_name, description))
}

fn lookup_aux_tool(name: &str, max_aux_agents: Option<usize>) -> Option<Tool> {
    if max_aux_agents.unwrap_or(0) == 0 {
        return None;
    }

    AUX_TOOLS
        .iter()
        .find(|(tool_name, _)| *tool_name == name)
        .map(|(tool_name, description)| build_simple_tool(tool_name, description))
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

pub(crate) fn is_code_editing_tool(name: &str) -> bool {
    CODE_EDITING_TOOL_NAMES.contains(&name)
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
            overrides: Default::default(),
        }
    }

    #[test]
    fn default_compute_tool_names_returns_allowlist() {
        let names = compute_tool_names(&empty_opts(), None);
        let expected: Vec<String> = CODE_EDITING_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn list_tools_matches_allowlist_by_default() {
        let opts = empty_opts();
        let tools = list_tools(&opts, None).expect("list tools");
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        for expected in CODE_EDITING_TOOL_NAMES {
            assert!(names.contains(expected));
        }
        for (admin, _) in ADMIN_ACTIONS {
            assert!(!names.contains(admin));
        }
    }

    #[test]
    fn expose_all_tools_includes_admin_catalog() {
        let mut opts = empty_opts();
        opts.expose_all_tools = true;
        let tools = list_tools(&opts, Some(2)).expect("list tools");
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        for expected in CODE_EDITING_TOOL_NAMES {
            assert!(names.contains(expected));
        }
        for (admin, _) in ADMIN_ACTIONS {
            assert!(names.contains(admin));
        }
        for (aux, _) in AUX_TOOLS {
            assert!(names.contains(aux));
        }
    }
}
