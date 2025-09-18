# MCP Server Integration Notes

This repository already ships with a native Rust implementation of the Model Context Protocol (MCP) server. The recent changes extend that server with:

- A flag-gated tool surface that mirrors the Codex action set (conversation lifecycle, auth, git helpers, exec utilities).
- Optional orchestration of auxiliary Codex CLI subprocesses that can be spawned, listed, and stopped via MCP tools when `--max-aux-agents` is set.
- CLI wiring (`codex mcp serve --expose-all-tools [--max-aux-agents=N]`) so that ChatGPT Developer Mode or any other MCP client can enable the extended experience when desired.

Key components:

- `codex-rs/mcp-server/src/message_processor.rs`: dispatches JSON-RPC requests and MCP tool calls, routing them into the corresponding Codex operations. New helper functions return typed results that are shared by both request paths.
- `codex-rs/mcp-server/src/codex_message_processor.rs`: houses the business logic for Codex operations (new conversation, resume, interrupt, auth flows, git helpers, exec, etc.). Each operation now has an `*_internal` variant returning `Result<..., JSONRPCErrorError>` for reuse.
- `codex-rs/mcp-server/src/tool_catalog.rs`: builds the list of MCP tools, conditionally adding the extended tool set and auxiliary-agent helpers.
- `codex-rs/mcp-server/src/aux_agents.rs`: manages bounded auxiliary agents, streaming their stdout/stderr back over MCP notifications and cleaning up on exit.

Usage quick start:

```bash
# Basic compatibility mode (legacy tools only)
codex mcp serve

# Full tool surface + auxiliary agents (up to 3 helpers)
codex mcp serve --expose-all-tools --max-aux-agents=3
```

To self-host the MCP server for ChatGPT Developer Mode, point the developer configuration at the `codex` binary with the flags above. The MCP inspector (`npx @modelcontextprotocol/inspector`) can be used to exercise the new tools and observe `codex/aux-agent/*` notifications.

