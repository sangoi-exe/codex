use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

use crate::McpServerOpts;
use crate::aux_agents::AuxAgentManager;
use crate::codex_message_processor::CodexMessageProcessor;
use crate::codex_message_processor::PendingInterrupt;
use crate::codex_tool_config::CodexToolCallParam;
use crate::codex_tool_config::CodexToolCallReplyParam;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use crate::outgoing_message::OutgoingMessageSender;
use crate::tool_catalog;
use codex_protocol::mcp_protocol::ArchiveConversationParams;
use codex_protocol::mcp_protocol::CancelLoginChatGptParams;
use codex_protocol::mcp_protocol::ClientRequest;
use codex_protocol::mcp_protocol::ConversationId;
use codex_protocol::mcp_protocol::ExecOneOffCommandParams;
use codex_protocol::mcp_protocol::GetAuthStatusParams;
use codex_protocol::mcp_protocol::GitDiffToRemoteParams;
use codex_protocol::mcp_protocol::InterruptConversationParams;
use codex_protocol::mcp_protocol::ListConversationsParams;
use codex_protocol::mcp_protocol::LoginApiKeyParams;
use codex_protocol::mcp_protocol::NewConversationParams;
use codex_protocol::mcp_protocol::ResumeConversationParams;
use codex_protocol::mcp_protocol::SendUserMessageParams;
use codex_protocol::mcp_protocol::SendUserTurnParams;
use codex_protocol::mcp_protocol::SetDefaultModelParams;

use codex_core::AuthManager;
use codex_core::ConversationManager;
use codex_core::config::Config;
use codex_core::default_client::USER_AGENT_SUFFIX;
use codex_core::default_client::get_codex_user_agent;
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
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
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
    server_opts: McpServerOpts,
    max_aux_agents: Option<usize>,
    aux_agents: Option<AuxAgentManager>,
}

#[derive(Debug, Deserialize)]
struct SpawnAuxAgentParams {
    prompt: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct StopAuxAgentParams {
    agent_id: Uuid,
}

#[derive(Debug, Deserialize, Default)]
struct ListAuxAgentsParams {}

#[derive(Debug, Deserialize)]
struct ReplyToolParams {
    prompt: String,
}

impl MessageProcessor {
    fn tool_success_message(
        message: impl Into<String>,
        structured: Option<serde_json::Value>,
    ) -> CallToolResult {
        CallToolResult {
            content: vec![ContentBlock::TextContent(TextContent {
                r#type: "text".to_string(),
                text: message.into(),
                annotations: None,
            })],
            is_error: None,
            structured_content: structured,
        }
    }

    fn tool_success_from_value(
        message: impl Into<String>,
        value: impl Serialize,
    ) -> CallToolResult {
        match serde_json::to_value(value) {
            Ok(val) => Self::tool_success_message(message, Some(val)),
            Err(err) => {
                Self::tool_error_message(format!("failed to serialize tool response: {err}"))
            }
        }
    }

    fn tool_error_message(message: impl Into<String>) -> CallToolResult {
        CallToolResult {
            content: vec![ContentBlock::TextContent(TextContent {
                r#type: "text".to_string(),
                text: message.into(),
                annotations: None,
            })],
            is_error: Some(true),
            structured_content: None,
        }
    }

    fn tool_error_from_rpc(error: JSONRPCErrorError) -> CallToolResult {
        Self::tool_error_message(error.message)
    }

    async fn handle_reply_tool(&self, id: RequestId, arguments: Option<serde_json::Value>) {
        let params = match self.parse_tool_arguments::<ReplyToolParams>("reply", arguments) {
            Ok(value) => value,
            Err(err) => {
                self.send_tool_call_result(&id, err).await;
                return;
            }
        };

        let structured = json!({ "echo": params.prompt });
        let result =
            Self::tool_success_message(format!("Reply sent: {}", params.prompt), Some(structured));
        self.send_tool_call_result(&id, result).await;
    }

    fn parse_tool_arguments<T: DeserializeOwned>(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<T, CallToolResult> {
        arguments
            .ok_or_else(|| Self::tool_error_message(format!("tool '{name}' requires arguments")))
            .and_then(|value| {
                serde_json::from_value(value).map_err(|err| {
                    Self::tool_error_message(format!(
                        "failed to parse arguments for tool '{name}': {err}"
                    ))
                })
            })
    }

    fn parse_tool_arguments_or_empty<T: DeserializeOwned>(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<T, CallToolResult> {
        let value = arguments.unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        serde_json::from_value(value).map_err(|err| {
            Self::tool_error_message(format!(
                "failed to parse arguments for tool '{name}': {err}"
            ))
        })
    }

    async fn send_tool_call_result(&self, id: &RequestId, result: CallToolResult) {
        self.send_response::<mcp_types::CallToolRequest>(id.clone(), result)
            .await;
    }

    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(
        outgoing: OutgoingMessageSender,
        codex_linux_sandbox_exe: Option<PathBuf>,
        config: Arc<Config>,
        server_opts: McpServerOpts,
        max_aux_agents: Option<usize>,
    ) -> Self {
        let outgoing = Arc::new(outgoing);
        let auth_manager = AuthManager::shared(config.codex_home.clone());
        let conversation_manager = Arc::new(ConversationManager::new(auth_manager.clone()));
        let aux_agents = max_aux_agents.and_then(|limit| {
            if limit == 0 {
                return None;
            }
            let exe = std::env::current_exe().ok()?;
            Some(AuxAgentManager::new(
                limit,
                exe,
                config.cwd.clone(),
                outgoing.clone(),
            ))
        });
        let codex_message_processor = CodexMessageProcessor::new(
            auth_manager,
            conversation_manager.clone(),
            outgoing.clone(),
            codex_linux_sandbox_exe.clone(),
            config.clone(),
            server_opts.clone(),
        );
        Self {
            codex_message_processor,
            outgoing,
            initialized: false,
            codex_linux_sandbox_exe,
            conversation_manager,
            running_requests_id_to_codex_uuid: Arc::new(Mutex::new(HashMap::new())),
            server_opts,
            max_aux_agents,
            aux_agents,
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
        match tool_catalog::list_tools(&self.server_opts, self.max_aux_agents) {
            Ok(tools) => {
                let result = ListToolsResult {
                    tools,
                    next_cursor: None,
                };

                self.send_response::<mcp_types::ListToolsRequest>(id, result)
                    .await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: err,
                    data: None,
                };
                self.outgoing.send_error(id, error).await;
            }
        }
    }

    async fn handle_call_tool(
        &self,
        id: RequestId,
        params: <mcp_types::CallToolRequest as mcp_types::ModelContextProtocolRequest>::Params,
    ) {
        tracing::info!("tools/call -> params: {:?}", params);
        let CallToolRequestParams { name, arguments } = params;

        match name.as_str() {
            "reply" => {
                self.handle_reply_tool(id, arguments).await;
            }
            "codex" => self.handle_tool_call_codex(id, arguments).await,
            "codex-reply" => {
                self.handle_tool_call_codex_session_reply(id, arguments)
                    .await
            }
            other if crate::tool_catalog::is_code_editing_tool(other) => {
                self.handle_extended_tool_call(name, id, arguments).await;
            }
            _ if self.server_opts.expose_all_tools => {
                self.handle_extended_tool_call(name, id, arguments).await;
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

    async fn handle_extended_tool_call(
        &self,
        name: String,
        id: RequestId,
        arguments: Option<serde_json::Value>,
    ) {
        match name.as_str() {
            "codex.newConversation" => {
                let params = match self
                    .parse_tool_arguments::<NewConversationParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .new_conversation_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result = Self::tool_success_from_value(
                            "Started new Codex conversation",
                            &response,
                        );
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.listConversations" => {
                let params = if let Some(value) = arguments.clone() {
                    match serde_json::from_value::<ListConversationsParams>(value) {
                        Ok(p) => p,
                        Err(err) => {
                            self.send_tool_call_result(
                                &id,
                                Self::tool_error_message(format!(
                                    "failed to parse arguments for tool '{name}': {err}"
                                )),
                            )
                            .await;
                            return;
                        }
                    }
                } else {
                    ListConversationsParams::default()
                };

                match self
                    .codex_message_processor
                    .list_conversations_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Listed Codex conversations", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.resumeConversation" => {
                let params = match self
                    .parse_tool_arguments::<ResumeConversationParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .resume_conversation_internal(params)
                    .await
                {
                    Ok((event, response)) => {
                        if let Some(event) = event {
                            self.outgoing.send_event_as_notification(&event, None).await;
                        }
                        let result =
                            Self::tool_success_from_value("Resumed Codex conversation", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.archiveConversation" => {
                let params = match self
                    .parse_tool_arguments::<ArchiveConversationParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .archive_conversation_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Archived Codex conversation", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.sendUserMessage" => {
                let params = match self
                    .parse_tool_arguments::<SendUserMessageParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .send_user_message_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result = Self::tool_success_from_value(
                            "Delivered user message to Codex conversation",
                            &response,
                        );
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.sendUserTurn" => {
                let params = match self
                    .parse_tool_arguments::<SendUserTurnParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .send_user_turn_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result = Self::tool_success_from_value(
                            "Submitted user turn to Codex conversation",
                            &response,
                        );
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.interruptConversation" => {
                let params = match self
                    .parse_tool_arguments::<InterruptConversationParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                if let Err(err) = self
                    .codex_message_processor
                    .schedule_interrupt(params.conversation_id, PendingInterrupt::Tool(id.clone()))
                    .await
                {
                    self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                        .await;
                }
                // Response will be sent when TurnAborted arrives.
            }
            "codex.gitDiffToRemote" => {
                let params = match self
                    .parse_tool_arguments::<GitDiffToRemoteParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .git_diff_to_origin_internal(params.cwd)
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Computed git diff to remote", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await;
                    }
                }
            }
            "codex.loginApiKey" => {
                let params = match self
                    .parse_tool_arguments::<LoginApiKeyParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .login_api_key_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result = Self::tool_success_from_value("Stored API key", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.loginChatGpt" => {
                match self.codex_message_processor.login_chatgpt_internal().await {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Initiated ChatGPT login", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.cancelLoginChatGpt" => {
                let params = match self
                    .parse_tool_arguments::<CancelLoginChatGptParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .cancel_login_chatgpt_internal(params.login_id)
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Cancelled ChatGPT login", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.logoutChatGpt" => {
                match self.codex_message_processor.logout_chatgpt_internal().await {
                    Ok(response) => {
                        let result = Self::tool_success_from_value("Logged out ChatGPT", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.getAuthStatus" => {
                let params = match self
                    .parse_tool_arguments_or_empty::<GetAuthStatusParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .get_auth_status_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Fetched auth status", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.getUserSavedConfig" => {
                match self
                    .codex_message_processor
                    .get_user_saved_config_internal()
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Fetched saved config", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.setDefaultModel" => {
                let params = match self
                    .parse_tool_arguments::<SetDefaultModelParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .set_default_model_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result =
                            Self::tool_success_from_value("Updated default model", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.getUserAgent" => match self.codex_message_processor.get_user_agent_internal() {
                Ok(response) => {
                    let result =
                        Self::tool_success_from_value("Fetched Codex user agent", &response);
                    self.send_tool_call_result(&id, result).await;
                }
                Err(err) => {
                    self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                        .await
                }
            },
            "codex.userInfo" => match self.codex_message_processor.get_user_info_internal().await {
                Ok(response) => {
                    let result = Self::tool_success_from_value("Fetched user info", &response);
                    self.send_tool_call_result(&id, result).await;
                }
                Err(err) => {
                    self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                        .await
                }
            },
            "codex.execCommand" => {
                let params = match self
                    .parse_tool_arguments::<ExecOneOffCommandParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match self
                    .codex_message_processor
                    .exec_one_off_command_internal(params)
                    .await
                {
                    Ok(response) => {
                        let result = Self::tool_success_from_value("Executed command", &response);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(&id, Self::tool_error_from_rpc(err))
                            .await
                    }
                }
            }
            "codex.spawnAuxAgent" => {
                let manager = match &self.aux_agents {
                    Some(mgr) => mgr.clone(),
                    None => {
                        self.send_tool_call_result(
                            &id,
                            Self::tool_error_message(
                                "auxiliary agents are disabled for this server",
                            ),
                        )
                        .await;
                        return;
                    }
                };
                let params = match self
                    .parse_tool_arguments::<SpawnAuxAgentParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match manager.spawn_agent(params.prompt, params.cwd).await {
                    Ok(result) => {
                        let result =
                            Self::tool_success_from_value("Spawned auxiliary agent", &result);
                        self.send_tool_call_result(&id, result).await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(
                            &id,
                            Self::tool_error_message(format!(
                                "failed to spawn auxiliary agent: {err}"
                            )),
                        )
                        .await;
                    }
                }
            }
            "codex.stopAuxAgent" => {
                let manager = match &self.aux_agents {
                    Some(mgr) => mgr.clone(),
                    None => {
                        self.send_tool_call_result(
                            &id,
                            Self::tool_error_message(
                                "auxiliary agents are disabled for this server",
                            ),
                        )
                        .await;
                        return;
                    }
                };
                let params = match self
                    .parse_tool_arguments::<StopAuxAgentParams>(&name, arguments.clone())
                {
                    Ok(p) => p,
                    Err(err) => {
                        self.send_tool_call_result(&id, err).await;
                        return;
                    }
                };

                match manager.stop_agent(params.agent_id).await {
                    Ok(()) => {
                        self.send_tool_call_result(
                            &id,
                            Self::tool_success_message(
                                format!("Stopped auxiliary agent {}", params.agent_id),
                                None,
                            ),
                        )
                        .await;
                    }
                    Err(err) => {
                        self.send_tool_call_result(
                            &id,
                            Self::tool_error_message(format!(
                                "failed to stop auxiliary agent: {err}"
                            )),
                        )
                        .await;
                    }
                }
            }
            "codex.listAuxAgents" => {
                let manager = match &self.aux_agents {
                    Some(mgr) => mgr.clone(),
                    None => {
                        self.send_tool_call_result(
                            &id,
                            Self::tool_error_message(
                                "auxiliary agents are disabled for this server",
                            ),
                        )
                        .await;
                        return;
                    }
                };
                let _params: ListAuxAgentsParams =
                    match self.parse_tool_arguments_or_empty(&name, arguments.clone()) {
                        Ok(p) => p,
                        Err(err) => {
                            self.send_tool_call_result(&id, err).await;
                            return;
                        }
                    };

                let agents = manager.list_agents().await;
                let result = Self::tool_success_from_value("Listed auxiliary agents", &agents);
                self.send_tool_call_result(&id, result).await;
            }
            _ => {
                let result = Self::tool_error_message(format!(
                    "Tool '{name}' is not yet implemented in the extended Codex surface"
                ));
                self.send_tool_call_result(&id, result).await;
            }
        }
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
