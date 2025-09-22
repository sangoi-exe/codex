use std::collections::HashMap;
use std::path::PathBuf;

use crate::codex_message_processor::CodexMessageProcessor;
use crate::codex_tool_config::CodexToolCallParam;
use crate::codex_tool_config::CodexToolCallReplyParam;
use crate::codex_tool_config::ExecCommandToolParam;
use crate::codex_tool_config::GitDiffToRemoteToolParam;
use crate::codex_tool_config::create_tool_for_apply_patch;
use crate::codex_tool_config::create_tool_for_code_search;
use crate::codex_tool_config::create_tool_for_codex_tool_call_param;
use crate::codex_tool_config::create_tool_for_codex_tool_call_reply_param;
use crate::codex_tool_config::create_tool_for_exec_command;
use crate::codex_tool_config::create_tool_for_git_diff_to_remote;
use crate::codex_tool_config::create_tool_for_read_file;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use crate::outgoing_message::OutgoingMessageSender;
use codex_file_search as file_search;
use codex_protocol::mcp_protocol::ClientRequest;
use codex_protocol::mcp_protocol::ConversationId;

use base64::Engine as _;
use codex_core::AuthManager;
use codex_core::CODEX_APPLY_PATCH_ARG1;
use codex_core::ConversationManager;
use codex_core::config::Config;
use codex_core::default_client::USER_AGENT_SUFFIX;
use codex_core::default_client::get_codex_user_agent;
use codex_core::exec::ExecParams;
use codex_core::exec_env::create_env;
use codex_core::get_platform_sandbox;
use codex_core::git_info::git_diff_to_remote;
use codex_core::protocol::Submission;
use mcp_types::CallToolRequestParams;
use mcp_types::CallToolResult;
use mcp_types::ClientRequest as McpClientRequest;
use mcp_types::ContentBlock;
use mcp_types::JSONRPCError;
use mcp_types::JSONRPCErrorError;
use mcp_types::JSONRPCNotification;
use mcp_types::JSONRPCRequest;
use mcp_types::JSONRPCResponse;
use mcp_types::ListToolsResult;
use mcp_types::ModelContextProtocolRequest;
use mcp_types::RequestId;
use mcp_types::ServerCapabilitiesTools;
use mcp_types::ServerNotification;
use mcp_types::TextContent;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task;

pub(crate) struct MessageProcessor {
    codex_message_processor: CodexMessageProcessor,
    outgoing: Arc<OutgoingMessageSender>,
    initialized: bool,
    codex_linux_sandbox_exe: Option<PathBuf>,
    conversation_manager: Arc<ConversationManager>,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ConversationId>>>,
    config: Arc<Config>,
    opts: crate::ServerOptions,
}

impl MessageProcessor {
    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(
        outgoing: OutgoingMessageSender,
        codex_linux_sandbox_exe: Option<PathBuf>,
        config: Arc<Config>,
        opts: crate::ServerOptions,
    ) -> Self {
        let outgoing = Arc::new(outgoing);
        let auth_manager = AuthManager::shared(config.codex_home.clone());
        let conversation_manager = Arc::new(ConversationManager::new(auth_manager.clone()));
        let codex_message_processor = CodexMessageProcessor::new(
            auth_manager,
            conversation_manager.clone(),
            outgoing.clone(),
            codex_linux_sandbox_exe.clone(),
            config.clone(),
        );
        Self {
            codex_message_processor,
            outgoing,
            initialized: false,
            codex_linux_sandbox_exe,
            conversation_manager,
            running_requests_id_to_codex_uuid: Arc::new(Mutex::new(HashMap::new())),
            config,
            opts,
        }
    }

    pub(crate) async fn process_request(&mut self, request: JSONRPCRequest) {
        if let Ok(request_json) = serde_json::to_value(request.clone())
            && let Ok(codex_request) = serde_json::from_value::<ClientRequest>(request_json)
        {
            // If the request is a Codex request, handle it with the Codex
            // message processor.
            self.codex_message_processor
                .process_request(codex_request)
                .await;
            return;
        }

        // Hold on to the ID so we can respond.
        let request_id = request.id.clone();

        let client_request = match McpClientRequest::try_from(request) {
            Ok(client_request) => client_request,
            Err(e) => {
                tracing::warn!("Failed to convert request: {e}");
                return;
            }
        };

        // Dispatch to a dedicated handler for each request type.
        match client_request {
            McpClientRequest::InitializeRequest(params) => {
                self.handle_initialize(request_id, params).await;
            }
            McpClientRequest::PingRequest(params) => {
                self.handle_ping(request_id, params).await;
            }
            McpClientRequest::ListResourcesRequest(params) => {
                self.handle_list_resources(params);
            }
            McpClientRequest::ListResourceTemplatesRequest(params) => {
                self.handle_list_resource_templates(params);
            }
            McpClientRequest::ReadResourceRequest(params) => {
                self.handle_read_resource(params);
            }
            McpClientRequest::SubscribeRequest(params) => {
                self.handle_subscribe(params);
            }
            McpClientRequest::UnsubscribeRequest(params) => {
                self.handle_unsubscribe(params);
            }
            McpClientRequest::ListPromptsRequest(params) => {
                self.handle_list_prompts(params);
            }
            McpClientRequest::GetPromptRequest(params) => {
                self.handle_get_prompt(params);
            }
            McpClientRequest::ListToolsRequest(params) => {
                self.handle_list_tools(request_id, params).await;
            }
            McpClientRequest::CallToolRequest(params) => {
                self.handle_call_tool(request_id, params).await;
            }
            McpClientRequest::SetLevelRequest(params) => {
                self.handle_set_level(params);
            }
            McpClientRequest::CompleteRequest(params) => {
                self.handle_complete(params);
            }
        }
    }

    /// Handle a standalone JSON-RPC response originating from the peer.
    pub(crate) async fn process_response(&mut self, response: JSONRPCResponse) {
        tracing::info!("<- response: {:?}", response);
        let JSONRPCResponse { id, result, .. } = response;
        self.outgoing.notify_client_response(id, result).await
    }

    /// Handle a fire-and-forget JSON-RPC notification.
    pub(crate) async fn process_notification(&mut self, notification: JSONRPCNotification) {
        let server_notification = match ServerNotification::try_from(notification) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Failed to convert notification: {e}");
                return;
            }
        };

        // Similar to requests, route each notification type to its own stub
        // handler so additional logic can be implemented incrementally.
        match server_notification {
            ServerNotification::CancelledNotification(params) => {
                self.handle_cancelled_notification(params).await;
            }
            ServerNotification::ProgressNotification(params) => {
                self.handle_progress_notification(params);
            }
            ServerNotification::ResourceListChangedNotification(params) => {
                self.handle_resource_list_changed(params);
            }
            ServerNotification::ResourceUpdatedNotification(params) => {
                self.handle_resource_updated(params);
            }
            ServerNotification::PromptListChangedNotification(params) => {
                self.handle_prompt_list_changed(params);
            }
            ServerNotification::ToolListChangedNotification(params) => {
                self.handle_tool_list_changed(params);
            }
            ServerNotification::LoggingMessageNotification(params) => {
                self.handle_logging_message(params);
            }
        }
    }

    /// Handle an error object received from the peer.
    pub(crate) fn process_error(&mut self, err: JSONRPCError) {
        tracing::error!("<- error: {:?}", err);
    }

    async fn handle_initialize(
        &mut self,
        id: RequestId,
        params: <mcp_types::InitializeRequest as ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("initialize -> params: {:?}", params);

        if self.initialized {
            // Already initialised: send JSON-RPC error response.
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "initialize called more than once".to_string(),
                data: None,
            };
            self.outgoing.send_error(id, error).await;
            return;
        }

        let client_info = params.client_info;
        let name = client_info.name;
        let version = client_info.version;
        let user_agent_suffix = format!("{name}; {version}");
        if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
            *suffix = Some(user_agent_suffix);
        }

        self.initialized = true;

        // Build a minimal InitializeResult. Fill with placeholders.
        let result = mcp_types::InitializeResult {
            capabilities: mcp_types::ServerCapabilities {
                completions: None,
                experimental: None,
                logging: None,
                prompts: None,
                resources: None,
                tools: Some(ServerCapabilitiesTools {
                    list_changed: Some(true),
                }),
            },
            instructions: None,
            protocol_version: params.protocol_version.clone(),
            server_info: mcp_types::Implementation {
                name: "codex-mcp-server".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: Some("Codex".to_string()),
                user_agent: Some(get_codex_user_agent()),
            },
        };

        self.send_response::<mcp_types::InitializeRequest>(id, result)
            .await;
    }

    async fn send_response<T>(&self, id: RequestId, result: T::Result)
    where
        T: ModelContextProtocolRequest,
    {
        self.outgoing.send_response(id, result).await;
    }

    async fn handle_ping(
        &self,
        id: RequestId,
        params: <mcp_types::PingRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("ping -> params: {:?}", params);
        let result = json!({});
        self.send_response::<mcp_types::PingRequest>(id, result)
            .await;
    }

    fn handle_list_resources(
        &self,
        params: <mcp_types::ListResourcesRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("resources/list -> params: {:?}", params);
    }

    fn handle_list_resource_templates(
        &self,
        params:
            <mcp_types::ListResourceTemplatesRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("resources/templates/list -> params: {:?}", params);
    }

    fn handle_read_resource(
        &self,
        params: <mcp_types::ReadResourceRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("resources/read -> params: {:?}", params);
    }

    fn handle_subscribe(
        &self,
        params: <mcp_types::SubscribeRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("resources/subscribe -> params: {:?}", params);
    }

    fn handle_unsubscribe(
        &self,
        params: <mcp_types::UnsubscribeRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("resources/unsubscribe -> params: {:?}", params);
    }

    fn handle_list_prompts(
        &self,
        params: <mcp_types::ListPromptsRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("prompts/list -> params: {:?}", params);
    }

    fn handle_get_prompt(
        &self,
        params: <mcp_types::GetPromptRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("prompts/get -> params: {:?}", params);
    }

    async fn handle_list_tools(
        &self,
        id: RequestId,
        params: <mcp_types::ListToolsRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::trace!("tools/list -> {params:?}");
        let tools = if self.opts.code_tools_only {
            vec![
                create_tool_for_exec_command(),
                create_tool_for_git_diff_to_remote(),
                create_tool_for_apply_patch(),
                create_tool_for_code_search(),
                create_tool_for_read_file(),
            ]
        } else {
            vec![
                create_tool_for_codex_tool_call_param(),
                create_tool_for_codex_tool_call_reply_param(),
            ]
        };
        let result = ListToolsResult {
            tools,
            next_cursor: None,
        };

        self.send_response::<mcp_types::ListToolsRequest>(id, result)
            .await;
    }

    async fn handle_call_tool(
        &self,
        id: RequestId,
        params: <mcp_types::CallToolRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("tools/call -> params: {:?}", params);
        let CallToolRequestParams { name, arguments } = params;

        match name.as_str() {
            "codex" => self.handle_tool_call_codex(id, arguments).await,
            "codex-reply" => {
                self.handle_tool_call_codex_session_reply(id, arguments)
                    .await
            }
            "reply" if !self.opts.code_tools_only => {
                self.handle_tool_call_codex_session_reply(id, arguments)
                    .await
            }
            "execCommand" if self.opts.code_tools_only => {
                self.handle_tool_exec_command(id, arguments).await
            }
            "gitDiffToRemote" if self.opts.code_tools_only => {
                self.handle_tool_git_diff_to_remote(id, arguments).await
            }
            "applyPatch" if self.opts.code_tools_only => {
                self.handle_tool_apply_patch(id, arguments).await
            }
            "codeSearch" if self.opts.code_tools_only => {
                self.handle_tool_code_search(id, arguments).await
            }
            "readFile" if self.opts.code_tools_only => {
                self.handle_tool_read_file(id, arguments).await
            }
            _ => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: format!("Unknown tool '{name}'"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(id, result)
                    .await;
            }
        }
    }

    async fn handle_tool_exec_command(&self, request_id: RequestId, arguments: Option<Value>) {
        let ExecCommandToolParam {
            command,
            timeout_ms,
            cwd,
        } = match arguments {
            Some(json_val) => match serde_json::from_value::<ExecCommandToolParam>(json_val) {
                Ok(params) => params,
                Err(e) => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_owned(),
                            text: format!("Failed to parse execCommand arguments: {e}"),
                            annotations: None,
                        })],
                        is_error: Some(true),
                        structured_content: None,
                    };
                    self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                        .await;
                    return;
                }
            },
            None => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "Missing arguments for execCommand; the `command` array is required."
                            .to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        if command.is_empty() {
            let result = CallToolResult {
                content: vec![ContentBlock::TextContent(TextContent {
                    r#type: "text".to_string(),
                    text: "execCommand: `command` must not be empty".to_string(),
                    annotations: None,
                })],
                is_error: Some(true),
                structured_content: None,
            };
            self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                .await;
            return;
        }

        let cfg = self.config.as_ref();
        let cwd_path = cwd.map(PathBuf::from).unwrap_or_else(|| cfg.cwd.clone());
        let env = create_env(&cfg.shell_environment_policy);
        let exec_params = ExecParams {
            command,
            cwd: cwd_path,
            timeout_ms,
            env,
            with_escalated_permissions: None,
            justification: None,
        };

        let effective_policy = cfg.sandbox_policy.clone();
        let sandbox_type = match &effective_policy {
            codex_core::protocol::SandboxPolicy::DangerFullAccess => {
                codex_core::exec::SandboxType::None
            }
            _ => get_platform_sandbox().unwrap_or(codex_core::exec::SandboxType::None),
        };

        let outgoing = self.outgoing.clone();
        let req_id = request_id;
        let sandbox_cwd = cfg.cwd.clone();
        let codex_linux_sandbox_exe = self.codex_linux_sandbox_exe.clone();

        tokio::spawn(async move {
            match codex_core::exec::process_exec_tool_call(
                exec_params,
                sandbox_type,
                &effective_policy,
                sandbox_cwd.as_path(),
                &codex_linux_sandbox_exe,
                None,
            )
            .await
            {
                Ok(output) => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_string(),
                            text: format!(
                                "exit_code={} (see structured_content)",
                                output.exit_code
                            ),
                            annotations: None,
                        })],
                        is_error: Some(false),
                        structured_content: Some(json!({
                            "exit_code": output.exit_code,
                            "stdout": output.stdout.text,
                            "stderr": output.stderr.text,
                        })),
                    };
                    outgoing.send_response(req_id, result).await;
                }
                Err(err) => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_string(),
                            text: format!("execCommand failed: {err}"),
                            annotations: None,
                        })],
                        is_error: Some(true),
                        structured_content: None,
                    };
                    outgoing.send_response(req_id, result).await;
                }
            }
        });
    }

    async fn handle_tool_git_diff_to_remote(
        &self,
        request_id: RequestId,
        arguments: Option<Value>,
    ) {
        let params = match arguments
            .and_then(|v| serde_json::from_value::<GitDiffToRemoteToolParam>(v).ok())
        {
            Some(p) => p,
            None => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "gitDiffToRemote: missing or invalid `cwd`".to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        let diff = git_diff_to_remote(&PathBuf::from(params.cwd)).await;
        match diff {
            Some(v) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "git diff attached in structured_content".to_string(),
                        annotations: None,
                    })],
                    is_error: Some(false),
                    structured_content: Some(json!({ "sha": v.sha, "diff": v.diff })),
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
            }
            None => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "failed to compute git diff to remote".to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
            }
        }
    }

    async fn handle_tool_apply_patch(&self, request_id: RequestId, arguments: Option<Value>) {
        #[derive(serde::Deserialize)]
        struct Args {
            patch: String,
            #[allow(dead_code)]
            cwd: Option<String>,
        }

        let Args { patch, cwd } =
            match arguments.and_then(|v| serde_json::from_value::<Args>(v).ok()) {
                Some(a) => a,
                None => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_string(),
                            text: "applyPatch: require { patch: string }".to_string(),
                            annotations: None,
                        })],
                        is_error: Some(true),
                        structured_content: None,
                    };
                    self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                        .await;
                    return;
                }
            };

        // Build an exec invocation that calls the current executable with the
        // secret CODEX_APPLY_PATCH_ARG1 flag so the arg0 path applies the patch
        // with the same sandbox enforcement as other execs.
        let path_to_codex = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "applyPatch: failed to resolve current executable".to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        let cfg = self.config.as_ref();
        let cwd_path = cwd.map(PathBuf::from).unwrap_or_else(|| cfg.cwd.clone());
        let exec_params = ExecParams {
            command: vec![
                path_to_codex.to_string_lossy().to_string(),
                CODEX_APPLY_PATCH_ARG1.to_string(),
                patch,
            ],
            cwd: cwd_path,
            timeout_ms: Some(120_000),
            env: std::collections::HashMap::new(),
            with_escalated_permissions: None,
            justification: None,
        };

        let effective_policy = cfg.sandbox_policy.clone();
        let sandbox_type = match &effective_policy {
            codex_core::protocol::SandboxPolicy::DangerFullAccess => {
                codex_core::exec::SandboxType::None
            }
            _ => get_platform_sandbox().unwrap_or(codex_core::exec::SandboxType::None),
        };

        let outgoing = self.outgoing.clone();
        let req_id = request_id;
        let codex_linux_sandbox_exe = self.codex_linux_sandbox_exe.clone();
        let sandbox_cwd = cfg.cwd.clone();

        tokio::spawn(async move {
            match codex_core::exec::process_exec_tool_call(
                exec_params,
                sandbox_type,
                &effective_policy,
                sandbox_cwd.as_path(),
                &codex_linux_sandbox_exe,
                None,
            )
            .await
            {
                Ok(output) => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_string(),
                            text: "applyPatch completed (see structured_content)".to_string(),
                            annotations: None,
                        })],
                        is_error: Some(false),
                        structured_content: Some(json!({
                            "exit_code": output.exit_code,
                            "stdout": output.stdout.text,
                            "stderr": output.stderr.text,
                        })),
                    };
                    outgoing.send_response(req_id, result).await;
                }
                Err(err) => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_string(),
                            text: format!("applyPatch failed: {err}"),
                            annotations: None,
                        })],
                        is_error: Some(true),
                        structured_content: None,
                    };
                    outgoing.send_response(req_id, result).await;
                }
            }
        });
    }

    async fn handle_tool_code_search(&self, request_id: RequestId, arguments: Option<Value>) {
        #[derive(serde::Deserialize)]
        struct Args {
            pattern: String,
            limit: Option<u32>,
            cwd: Option<String>,
            exclude: Option<Vec<String>>,
            compute_indices: Option<bool>,
        }

        let Args {
            pattern,
            limit,
            cwd,
            exclude,
            compute_indices,
        } = match arguments.and_then(|v| serde_json::from_value::<Args>(v).ok()) {
            Some(a) => a,
            None => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "codeSearch: require { pattern: string }".to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        let cfg = self.config.as_ref();
        let search_dir = cwd.map(PathBuf::from).unwrap_or_else(|| cfg.cwd.clone());
        let limit_nz = std::num::NonZero::new(limit.unwrap_or(200).max(1) as usize)
            .unwrap_or(std::num::NonZero::new(200usize).unwrap());
        let threads_nz = std::num::NonZero::new(4usize).unwrap();
        let exclude = exclude.unwrap_or_default();
        let compute_indices = compute_indices.unwrap_or(false);

        // codex-file-search uses an AtomicBool cancel flag; create one.
        let cancel_atomic = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let res = file_search::run(
            &pattern,
            limit_nz,
            &search_dir,
            exclude,
            threads_nz,
            cancel_atomic,
            compute_indices,
        );

        match res {
            Ok(r) => {
                let matches: Vec<serde_json::Value> = r
                    .matches
                    .into_iter()
                    .map(|m| {
                        let indices = m.indices.map(|idx| {
                            idx.into_iter()
                                .map(serde_json::Value::from)
                                .collect::<Vec<_>>()
                        });
                        json!({
                            "path": m.path,
                            "score": m.score,
                            "indices": indices,
                        })
                    })
                    .collect();
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: format!(
                            "{} matches (showing up to {}): codeSearch completed",
                            r.total_match_count, limit_nz
                        ),
                        annotations: None,
                    })],
                    is_error: Some(false),
                    structured_content: Some(json!({
                        "total": r.total_match_count,
                        "matches": matches,
                    })),
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
            }
            Err(err) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: format!("codeSearch failed: {err}"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
            }
        }
    }

    async fn handle_tool_read_file(&self, request_id: RequestId, arguments: Option<Value>) {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            start: Option<u64>,
            max_bytes: Option<u64>,
        }

        let Args {
            path,
            start,
            max_bytes,
        } = match arguments.and_then(|v| serde_json::from_value::<Args>(v).ok()) {
            Some(a) => a,
            None => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: "readFile: require { path: string }".to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        let cfg = self.config.as_ref();
        let root = match std::fs::canonicalize(&cfg.cwd) {
            Ok(p) => p,
            Err(err) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: format!("readFile: failed to resolve workspace root: {err}"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        // Resolve candidate path relative to root if needed, then canonicalize.
        let cand = {
            let p = PathBuf::from(&path);
            if p.is_absolute() { p } else { root.join(p) }
        };
        let target = match std::fs::canonicalize(&cand) {
            Ok(p) => p,
            Err(err) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: format!("readFile: path not found or invalid: {err}"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        // Enforce workspace boundary.
        if !target.starts_with(&root) {
            let result = CallToolResult {
                content: vec![ContentBlock::TextContent(TextContent {
                    r#type: "text".to_string(),
                    text: "readFile: path must be inside the workspace root".to_string(),
                    annotations: None,
                })],
                is_error: Some(true),
                structured_content: None,
            };
            self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                .await;
            return;
        }

        // Read and slice.
        let max_cap: u64 = 5_000_000; // 5MB cap
        let default_max: u64 = 200_000; // 200KB default
        let max_read = max_bytes.unwrap_or(default_max).min(max_cap) as usize;
        let start_off = start.unwrap_or(0) as usize;

        let bytes = match std::fs::read(&target) {
            Ok(b) => b,
            Err(err) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text: format!("readFile: failed to read file: {err}"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        let total = bytes.len();
        let start_off = start_off.min(total);
        let end = (start_off + max_read).min(total);
        let slice = &bytes[start_off..end];

        let (body, encoding) = match std::str::from_utf8(slice) {
            Ok(s) => (json!({"text": s}), "utf-8".to_string()),
            Err(_) => (
                json!({"base64": base64::engine::general_purpose::STANDARD.encode(slice)}),
                "base64".to_string(),
            ),
        };

        let result = CallToolResult {
            content: vec![ContentBlock::TextContent(TextContent {
                r#type: "text".to_string(),
                text: format!(
                    "readFile: {} bytes from {} (offset {} of {})",
                    slice.len(),
                    target.display(),
                    start_off,
                    total
                ),
                annotations: None,
            })],
            is_error: Some(false),
            structured_content: Some(json!({
                "path": target.to_string_lossy(),
                "encoding": encoding,
                "start": start_off,
                "read_bytes": slice.len(),
                "total_bytes": total,
                "content": body,
            })),
        };
        self.send_response::<mcp_types::CallToolRequest>(request_id, result)
            .await;
    }
    async fn handle_tool_call_codex(&self, id: RequestId, arguments: Option<serde_json::Value>) {
        let (initial_prompt, config): (String, Config) = match arguments {
            Some(json_val) => match serde_json::from_value::<CodexToolCallParam>(json_val) {
                Ok(tool_cfg) => match tool_cfg.into_config(self.codex_linux_sandbox_exe.clone()) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        let result = CallToolResult {
                            content: vec![ContentBlock::TextContent(TextContent {
                                r#type: "text".to_owned(),
                                text: format!(
                                    "Failed to load Codex configuration from overrides: {e}"
                                ),
                                annotations: None,
                            })],
                            is_error: Some(true),
                            structured_content: None,
                        };
                        self.send_response::<mcp_types::CallToolRequest>(id, result)
                            .await;
                        return;
                    }
                },
                Err(e) => {
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_owned(),
                            text: format!("Failed to parse configuration for Codex tool: {e}"),
                            annotations: None,
                        })],
                        is_error: Some(true),
                        structured_content: None,
                    };
                    self.send_response::<mcp_types::CallToolRequest>(id, result)
                        .await;
                    return;
                }
            },
            None => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_string(),
                        text:
                            "Missing arguments for codex tool-call; the `prompt` field is required."
                                .to_string(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(id, result)
                    .await;
                return;
            }
        };

        // Clone outgoing and server to move into async task.
        let outgoing = self.outgoing.clone();
        let conversation_manager = self.conversation_manager.clone();
        let running_requests_id_to_codex_uuid = self.running_requests_id_to_codex_uuid.clone();

        // Spawn an async task to handle the Codex session so that we do not
        // block the synchronous message-processing loop.
        task::spawn(async move {
            // Run the Codex session and stream events back to the client.
            crate::codex_tool_runner::run_codex_tool_session(
                id,
                initial_prompt,
                config,
                outgoing,
                conversation_manager,
                running_requests_id_to_codex_uuid,
            )
            .await;
        });
    }

    async fn handle_tool_call_codex_session_reply(
        &self,
        request_id: RequestId,
        arguments: Option<serde_json::Value>,
    ) {
        tracing::info!("tools/call -> params: {:?}", arguments);

        // parse arguments
        let CodexToolCallReplyParam {
            conversation_id,
            prompt,
        } = match arguments {
            Some(json_val) => match serde_json::from_value::<CodexToolCallReplyParam>(json_val) {
                Ok(params) => params,
                Err(e) => {
                    tracing::error!("Failed to parse Codex tool call reply parameters: {e}");
                    let result = CallToolResult {
                        content: vec![ContentBlock::TextContent(TextContent {
                            r#type: "text".to_owned(),
                            text: format!("Failed to parse configuration for Codex tool: {e}"),
                            annotations: None,
                        })],
                        is_error: Some(true),
                        structured_content: None,
                    };
                    self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                        .await;
                    return;
                }
            },
            None => {
                tracing::error!(
                    "Missing arguments for codex-reply tool-call; the `conversation_id` and `prompt` fields are required."
                );
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_owned(),
                        text: "Missing arguments for codex-reply tool-call; the `conversation_id` and `prompt` fields are required.".to_owned(),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };
        let conversation_id = match ConversationId::from_string(&conversation_id) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to parse conversation_id: {e}");
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_owned(),
                        text: format!("Failed to parse conversation_id: {e}"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                self.send_response::<mcp_types::CallToolRequest>(request_id, result)
                    .await;
                return;
            }
        };

        // Clone outgoing to move into async task.
        let outgoing = self.outgoing.clone();
        let running_requests_id_to_codex_uuid = self.running_requests_id_to_codex_uuid.clone();

        let codex = match self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
        {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("Session not found for conversation_id: {conversation_id}");
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_owned(),
                        text: format!("Session not found for conversation_id: {conversation_id}"),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                outgoing.send_response(request_id, result).await;
                return;
            }
        };

        // Spawn the long-running reply handler.
        tokio::spawn({
            let outgoing = outgoing.clone();
            let prompt = prompt.clone();
            let running_requests_id_to_codex_uuid = running_requests_id_to_codex_uuid.clone();

            async move {
                crate::codex_tool_runner::run_codex_tool_session_reply(
                    codex,
                    outgoing,
                    request_id,
                    prompt,
                    running_requests_id_to_codex_uuid,
                    conversation_id,
                )
                .await;
            }
        });
    }

    fn handle_set_level(
        &self,
        params: <mcp_types::SetLevelRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("logging/setLevel -> params: {:?}", params);
    }

    fn handle_complete(
        &self,
        params: <mcp_types::CompleteRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("completion/complete -> params: {:?}", params);
    }

    // ---------------------------------------------------------------------
    // Notification handlers
    // ---------------------------------------------------------------------

    async fn handle_cancelled_notification(
        &self,
        params: <mcp_types::CancelledNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        let request_id = params.request_id;
        // Create a stable string form early for logging and submission id.
        let request_id_string = match &request_id {
            RequestId::String(s) => s.clone(),
            RequestId::Integer(i) => i.to_string(),
        };

        // Obtain the conversation id while holding the first lock, then release.
        let conversation_id = {
            let map_guard = self.running_requests_id_to_codex_uuid.lock().await;
            match map_guard.get(&request_id) {
                Some(id) => *id,
                None => {
                    tracing::warn!("Session not found for request_id: {}", request_id_string);
                    return;
                }
            }
        };
        tracing::info!("conversation_id: {conversation_id}");

        // Obtain the Codex conversation from the server.
        let codex_arc = match self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
        {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("Session not found for conversation_id: {conversation_id}");
                return;
            }
        };

        // Submit interrupt to Codex.
        let err = codex_arc
            .submit_with_id(Submission {
                id: request_id_string,
                op: codex_core::protocol::Op::Interrupt,
            })
            .await;
        if let Err(e) = err {
            tracing::error!("Failed to submit interrupt to Codex: {e}");
            return;
        }
        // unregister the id so we don't keep it in the map
        self.running_requests_id_to_codex_uuid
            .lock()
            .await
            .remove(&request_id);
    }

    fn handle_progress_notification(
        &self,
        params: <mcp_types::ProgressNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        tracing::info!("notifications/progress -> params: {:?}", params);
    }

    fn handle_resource_list_changed(
        &self,
        params: <mcp_types::ResourceListChangedNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        tracing::info!(
            "notifications/resources/list_changed -> params: {:?}",
            params
        );
    }

    fn handle_resource_updated(
        &self,
        params: <mcp_types::ResourceUpdatedNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        tracing::info!("notifications/resources/updated -> params: {:?}", params);
    }

    fn handle_prompt_list_changed(
        &self,
        params: <mcp_types::PromptListChangedNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        tracing::info!("notifications/prompts/list_changed -> params: {:?}", params);
    }

    fn handle_tool_list_changed(
        &self,
        params: <mcp_types::ToolListChangedNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        tracing::info!("notifications/tools/list_changed -> params: {:?}", params);
    }

    fn handle_logging_message(
        &self,
        params: <mcp_types::LoggingMessageNotification as mcp_types::ModelContextProtocolNotification>::Params,
    ) {
        tracing::info!("notifications/message -> params: {:?}", params);
    }
}
