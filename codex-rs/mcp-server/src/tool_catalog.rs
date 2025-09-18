use mcp_types::Tool;
use mcp_types::ToolInputSchema;

use crate::McpServerFeatureFlags;
use crate::codex_tool_config::create_tool_for_codex_tool_call_param;
use crate::codex_tool_config::create_tool_for_codex_tool_call_reply_param;

pub fn list_tools(flags: &McpServerFeatureFlags) -> Vec<Tool> {
    let mut tools = vec![
        create_tool_for_codex_tool_call_param(),
        create_tool_for_codex_tool_call_reply_param(),
    ];

    if flags.expose_all_tools {
        tools.extend(full_action_surface(flags));
    }

    tools
}

fn full_action_surface(flags: &McpServerFeatureFlags) -> Vec<Tool> {
    const TOOLS: &[(&str, &str)] = &[
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
    let mut list: Vec<Tool> = TOOLS
        .iter()
        .map(|(name, desc)| build_simple_tool(name, desc))
        .collect();

    if flags.max_aux_agents.unwrap_or(0) > 0 {
        for (name, desc) in [
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
        ] {
            list.push(build_simple_tool(name, desc));
        }
    }

    list
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
