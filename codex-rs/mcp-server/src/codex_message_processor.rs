use crate::McpServerOpts;
use crate::error_code::INTERNAL_ERROR_CODE;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use crate::json_to_toml::json_to_toml;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotification;
use codex_core::AuthManager;
use codex_core::CodexConversation;
use codex_core::ConversationManager;
use codex_core::Cursor as RolloutCursor;
use codex_core::NewConversation;
use codex_core::RolloutRecorder;
use codex_core::SessionMeta;
use codex_core::auth::CLIENT_ID;
use codex_core::auth::get_auth_file;
use codex_core::auth::login_with_api_key;
use codex_core::auth::try_read_auth_json;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_core::config::ConfigToml;
use codex_core::config::load_config_as_toml;
use codex_core::config_edit::CONFIG_KEY_EFFORT;
use codex_core::config_edit::CONFIG_KEY_MODEL;
use codex_core::config_edit::persist_overrides_and_clear_if_none;
use codex_core::default_client::get_codex_user_agent;
use codex_core::exec::ExecParams;
use codex_core::exec_env::create_env;
use codex_core::get_platform_sandbox;
use codex_core::git_info::git_diff_to_remote;
use codex_core::protocol::ApplyPatchApprovalRequestEvent;
use codex_core::protocol::Event;
use codex_core::protocol::EventMsg;
use codex_core::protocol::ExecApprovalRequestEvent;
use codex_core::protocol::InputItem as CoreInputItem;
use codex_core::protocol::Op;
use codex_core::protocol::ReviewDecision;
use codex_login::ServerOptions as LoginServerOptions;
use codex_login::ShutdownHandle;
use codex_login::run_login_server;
use codex_protocol::mcp_protocol::APPLY_PATCH_APPROVAL_METHOD;
use codex_protocol::mcp_protocol::AddConversationListenerParams;
use codex_protocol::mcp_protocol::AddConversationSubscriptionResponse;
use codex_protocol::mcp_protocol::ApplyPatchApprovalParams;
use codex_protocol::mcp_protocol::ApplyPatchApprovalResponse;
use codex_protocol::mcp_protocol::ArchiveConversationParams;
use codex_protocol::mcp_protocol::ArchiveConversationResponse;
use codex_protocol::mcp_protocol::AuthStatusChangeNotification;
use codex_protocol::mcp_protocol::ClientRequest;
use codex_protocol::mcp_protocol::ConversationId;
use codex_protocol::mcp_protocol::ConversationSummary;
use codex_protocol::mcp_protocol::EXEC_COMMAND_APPROVAL_METHOD;
use codex_protocol::mcp_protocol::ExecArbitraryCommandResponse;
use codex_protocol::mcp_protocol::ExecCommandApprovalParams;
use codex_protocol::mcp_protocol::ExecCommandApprovalResponse;
use codex_protocol::mcp_protocol::ExecOneOffCommandParams;
use codex_protocol::mcp_protocol::GetUserAgentResponse;
use codex_protocol::mcp_protocol::GetUserSavedConfigResponse;
use codex_protocol::mcp_protocol::GitDiffToRemoteResponse;
use codex_protocol::mcp_protocol::InputItem as WireInputItem;
use codex_protocol::mcp_protocol::InterruptConversationParams;
use codex_protocol::mcp_protocol::InterruptConversationResponse;
use codex_protocol::mcp_protocol::ListConversationsParams;
use codex_protocol::mcp_protocol::ListConversationsResponse;
use codex_protocol::mcp_protocol::LoginApiKeyParams;
use codex_protocol::mcp_protocol::LoginApiKeyResponse;
use codex_protocol::mcp_protocol::LoginChatGptCompleteNotification;
use codex_protocol::mcp_protocol::LoginChatGptResponse;
use codex_protocol::mcp_protocol::NewConversationParams;
use codex_protocol::mcp_protocol::NewConversationResponse;
use codex_protocol::mcp_protocol::RemoveConversationListenerParams;
use codex_protocol::mcp_protocol::RemoveConversationSubscriptionResponse;
use codex_protocol::mcp_protocol::ResumeConversationParams;
use codex_protocol::mcp_protocol::ResumeConversationResponse;
use codex_protocol::mcp_protocol::SendUserMessageParams;
use codex_protocol::mcp_protocol::SendUserMessageResponse;
use codex_protocol::mcp_protocol::SendUserTurnParams;
use codex_protocol::mcp_protocol::SendUserTurnResponse;
use codex_protocol::mcp_protocol::ServerNotification;
use codex_protocol::mcp_protocol::SetDefaultModelParams;
use codex_protocol::mcp_protocol::SetDefaultModelResponse;
use codex_protocol::mcp_protocol::UserInfoResponse;
use codex_protocol::mcp_protocol::UserSavedConfig;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InputMessageKind;
use codex_protocol::protocol::USER_MESSAGE_BEGIN;
use mcp_types::CallToolResult;
use mcp_types::ContentBlock;
use mcp_types::JSONRPCErrorError;
use mcp_types::RequestId;
use mcp_types::TextContent;
use serde_json::json;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::error;
use tracing::info;
use tracing::warn;
use uuid::Uuid;

// Duration before a ChatGPT login attempt is abandoned.
const LOGIN_CHATGPT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct ActiveLogin {
    shutdown_handle: ShutdownHandle,
    login_id: Uuid,
}

impl ActiveLogin {
    fn drop(&self) {
        self.shutdown_handle.shutdown();
    }
}

#[derive(Clone, Debug)]
pub(crate) enum PendingInterrupt {
    JsonRpc(RequestId),
    Tool(RequestId),
}

/// Handles JSON-RPC messages for Codex conversations.
pub(crate) struct CodexMessageProcessor {
    auth_manager: Arc<AuthManager>,
    conversation_manager: Arc<ConversationManager>,
    outgoing: Arc<OutgoingMessageSender>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    config: Arc<Config>,
    conversation_listeners: HashMap<Uuid, oneshot::Sender<()>>,
    active_login: Arc<Mutex<Option<ActiveLogin>>>,
    // Queue of pending interrupt requests per conversation. We reply when TurnAborted arrives.
    pending_interrupts: Arc<Mutex<HashMap<ConversationId, Vec<PendingInterrupt>>>>,
    _server_opts: McpServerOpts,
}

impl CodexMessageProcessor {
    pub fn new(
        auth_manager: Arc<AuthManager>,
        conversation_manager: Arc<ConversationManager>,
        outgoing: Arc<OutgoingMessageSender>,
        codex_linux_sandbox_exe: Option<PathBuf>,
        config: Arc<Config>,
        server_opts: McpServerOpts,
    ) -> Self {
        Self {
            auth_manager,
            conversation_manager,
            outgoing,
            codex_linux_sandbox_exe,
            config,
            conversation_listeners: HashMap::new(),
            active_login: Arc::new(Mutex::new(None)),
            pending_interrupts: Arc::new(Mutex::new(HashMap::new())),
            _server_opts: server_opts,
        }
    }

    pub async fn process_request(&mut self, request: ClientRequest) {
        match request {
            ClientRequest::NewConversation { request_id, params } => {
                // Do not tokio::spawn() to process new_conversation()
                // asynchronously because we need to ensure the conversation is
                // created before processing any subsequent messages.
                self.process_new_conversation(request_id, params).await;
            }
            ClientRequest::ListConversations { request_id, params } => {
                self.handle_list_conversations(request_id, params).await;
            }
            ClientRequest::ResumeConversation { request_id, params } => {
                self.handle_resume_conversation(request_id, params).await;
            }
            ClientRequest::ArchiveConversation { request_id, params } => {
                self.archive_conversation(request_id, params).await;
            }
            ClientRequest::SendUserMessage { request_id, params } => {
                self.send_user_message(request_id, params).await;
            }
            ClientRequest::SendUserTurn { request_id, params } => {
                self.send_user_turn(request_id, params).await;
            }
            ClientRequest::InterruptConversation { request_id, params } => {
                self.interrupt_conversation(request_id, params).await;
            }
            ClientRequest::AddConversationListener { request_id, params } => {
                self.add_conversation_listener(request_id, params).await;
            }
            ClientRequest::RemoveConversationListener { request_id, params } => {
                self.remove_conversation_listener(request_id, params).await;
            }
            ClientRequest::GitDiffToRemote { request_id, params } => {
                self.git_diff_to_origin(request_id, params.cwd).await;
            }
            ClientRequest::LoginApiKey { request_id, params } => {
                self.login_api_key(request_id, params).await;
            }
            ClientRequest::LoginChatGpt { request_id } => {
                self.login_chatgpt(request_id).await;
            }
            ClientRequest::CancelLoginChatGpt { request_id, params } => {
                self.cancel_login_chatgpt(request_id, params.login_id).await;
            }
            ClientRequest::LogoutChatGpt { request_id } => {
                self.logout_chatgpt(request_id).await;
            }
            ClientRequest::GetAuthStatus { request_id, params } => {
                self.get_auth_status(request_id, params).await;
            }
            ClientRequest::GetUserSavedConfig { request_id } => {
                self.get_user_saved_config(request_id).await;
            }
            ClientRequest::SetDefaultModel { request_id, params } => {
                self.set_default_model(request_id, params).await;
            }
            ClientRequest::GetUserAgent { request_id } => {
                self.get_user_agent(request_id).await;
            }
            ClientRequest::UserInfo { request_id } => {
                self.get_user_info(request_id).await;
            }
            ClientRequest::ExecOneOffCommand { request_id, params } => {
                self.exec_one_off_command(request_id, params).await;
            }
        }
    }

    async fn login_api_key(&mut self, request_id: RequestId, params: LoginApiKeyParams) {
        match self.login_api_key_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn login_api_key_internal(
        &self,
        params: LoginApiKeyParams,
    ) -> Result<LoginApiKeyResponse, JSONRPCErrorError> {
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                active.drop();
            }
        }

        match login_with_api_key(&self.config.codex_home, &params.api_key) {
            Ok(()) => {
                self.auth_manager.reload();
                let payload = AuthStatusChangeNotification {
                    auth_method: self.auth_manager.auth().map(|auth| auth.mode),
                };
                self.outgoing
                    .send_server_notification(ServerNotification::AuthStatusChange(payload))
                    .await;
                Ok(LoginApiKeyResponse {})
            }
            Err(err) => Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to save api key: {err}"),
                data: None,
            }),
        }
    }

    async fn login_chatgpt(&mut self, request_id: RequestId) {
        match self.login_chatgpt_internal().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn login_chatgpt_internal(
        &self,
    ) -> Result<LoginChatGptResponse, JSONRPCErrorError> {
        let config = self.config.as_ref();

        let opts = LoginServerOptions {
            open_browser: false,
            ..LoginServerOptions::new(config.codex_home.clone(), CLIENT_ID.to_string())
        };

        let server = run_login_server(opts).map_err(|err| JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message: format!("failed to start login server: {err}"),
            data: None,
        })?;

        let login_id = Uuid::new_v4();
        let shutdown_handle = server.cancel_handle();

        {
            let mut guard = self.active_login.lock().await;
            if let Some(existing) = guard.take() {
                existing.drop();
            }
            *guard = Some(ActiveLogin {
                shutdown_handle: shutdown_handle.clone(),
                login_id,
            });
        }

        let response = LoginChatGptResponse {
            login_id,
            auth_url: server.auth_url.clone(),
        };

        let outgoing_clone = self.outgoing.clone();
        let active_login = self.active_login.clone();
        let auth_manager = self.auth_manager.clone();
        tokio::spawn(async move {
            let (success, error_msg) = match tokio::time::timeout(
                LOGIN_CHATGPT_TIMEOUT,
                server.block_until_done(),
            )
            .await
            {
                Ok(Ok(())) => (true, None),
                Ok(Err(err)) => (false, Some(format!("Login server error: {err}"))),
                Err(_elapsed) => {
                    shutdown_handle.shutdown();
                    (false, Some("Login timed out".to_string()))
                }
            };
            let payload = LoginChatGptCompleteNotification {
                login_id,
                success,
                error: error_msg,
            };
            outgoing_clone
                .send_server_notification(ServerNotification::LoginChatGptComplete(payload))
                .await;

            if success {
                auth_manager.reload();
                let auth_method = auth_manager.auth().map(|a| a.mode);
                outgoing_clone
                    .send_server_notification(ServerNotification::AuthStatusChange(
                        AuthStatusChangeNotification { auth_method },
                    ))
                    .await;
            }

            let mut guard = active_login.lock().await;
            if guard.as_ref().map(|l| l.login_id) == Some(login_id) {
                *guard = None;
            }
        });

        Ok(response)
    }

    async fn cancel_login_chatgpt(&mut self, request_id: RequestId, login_id: Uuid) {
        match self.cancel_login_chatgpt_internal(login_id).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn cancel_login_chatgpt_internal(
        &self,
        login_id: Uuid,
    ) -> Result<codex_protocol::mcp_protocol::CancelLoginChatGptResponse, JSONRPCErrorError> {
        let mut guard = self.active_login.lock().await;
        if guard.as_ref().map(|l| l.login_id) == Some(login_id) {
            if let Some(active) = guard.take() {
                active.drop();
            }
            Ok(codex_protocol::mcp_protocol::CancelLoginChatGptResponse {})
        } else {
            Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("login id not found: {login_id}"),
                data: None,
            })
        }
    }

    async fn logout_chatgpt(&mut self, request_id: RequestId) {
        match self.logout_chatgpt_internal().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn logout_chatgpt_internal(
        &self,
    ) -> Result<codex_protocol::mcp_protocol::LogoutChatGptResponse, JSONRPCErrorError> {
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                active.drop();
            }
        }

        self.auth_manager
            .logout()
            .map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("logout failed: {err}"),
                data: None,
            })?;

        let current_auth_method = self.auth_manager.auth().map(|auth| auth.mode);
        let payload = AuthStatusChangeNotification {
            auth_method: current_auth_method,
        };
        self.outgoing
            .send_server_notification(ServerNotification::AuthStatusChange(payload))
            .await;

        Ok(codex_protocol::mcp_protocol::LogoutChatGptResponse {})
    }

    async fn get_auth_status(
        &self,
        request_id: RequestId,
        params: codex_protocol::mcp_protocol::GetAuthStatusParams,
    ) {
        match self.get_auth_status_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn get_auth_status_internal(
        &self,
        params: codex_protocol::mcp_protocol::GetAuthStatusParams,
    ) -> Result<codex_protocol::mcp_protocol::GetAuthStatusResponse, JSONRPCErrorError> {
        let include_token = params.include_token.unwrap_or(false);
        let do_refresh = params.refresh_token.unwrap_or(false);

        if do_refresh && let Err(err) = self.auth_manager.refresh_token().await {
            tracing::warn!("failed to refresh token while getting auth status: {err}");
        }

        // Determine whether auth is required based on the active model provider.
        // If a custom provider is configured with `requires_openai_auth == false`,
        // then no auth step is required; otherwise, default to requiring auth.
        let requires_openai_auth = self.config.model_provider.requires_openai_auth;

        let response = if !requires_openai_auth {
            codex_protocol::mcp_protocol::GetAuthStatusResponse {
                auth_method: None,
                auth_token: None,
                requires_openai_auth: Some(false),
            }
        } else {
            match self.auth_manager.auth() {
                Some(auth) => {
                    let auth_mode = auth.mode;
                    let (reported_auth_method, token_opt) = match auth.get_token().await {
                        Ok(token) if !token.is_empty() => {
                            let tok = if include_token { Some(token) } else { None };
                            (Some(auth_mode), tok)
                        }
                        Ok(_) => (None, None),
                        Err(err) => {
                            tracing::warn!("failed to get token for auth status: {err}");
                            (None, None)
                        }
                    };
                    codex_protocol::mcp_protocol::GetAuthStatusResponse {
                        auth_method: reported_auth_method,
                        auth_token: token_opt,
                        requires_openai_auth: Some(true),
                    }
                }
                None => codex_protocol::mcp_protocol::GetAuthStatusResponse {
                    auth_method: None,
                    auth_token: None,
                    requires_openai_auth: Some(true),
                },
            }
        };

        Ok(response)
    }

    async fn get_user_agent(&self, request_id: RequestId) {
        match self.get_user_agent_internal() {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) fn get_user_agent_internal(
        &self,
    ) -> Result<GetUserAgentResponse, JSONRPCErrorError> {
        Ok(GetUserAgentResponse {
            user_agent: get_codex_user_agent(),
        })
    }

    async fn get_user_saved_config(&self, request_id: RequestId) {
        match self.get_user_saved_config_internal().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn get_user_saved_config_internal(
        &self,
    ) -> Result<GetUserSavedConfigResponse, JSONRPCErrorError> {
        let toml_value =
            load_config_as_toml(&self.config.codex_home).map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to load config.toml: {err}"),
                data: None,
            })?;

        let cfg: ConfigToml = toml_value.try_into().map_err(|err| JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message: format!("failed to parse config.toml: {err}"),
            data: None,
        })?;

        let user_saved_config: UserSavedConfig = cfg.into();

        Ok(GetUserSavedConfigResponse {
            config: user_saved_config,
        })
    }

    async fn get_user_info(&self, request_id: RequestId) {
        match self.get_user_info_internal().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn get_user_info_internal(
        &self,
    ) -> Result<UserInfoResponse, JSONRPCErrorError> {
        let auth_path = get_auth_file(&self.config.codex_home);
        let alleged_user_email = try_read_auth_json(&auth_path)
            .ok()
            .and_then(|auth| auth.tokens.and_then(|t| t.id_token.email));

        Ok(UserInfoResponse { alleged_user_email })
    }

    async fn set_default_model(&self, request_id: RequestId, params: SetDefaultModelParams) {
        match self.set_default_model_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn set_default_model_internal(
        &self,
        params: SetDefaultModelParams,
    ) -> Result<SetDefaultModelResponse, JSONRPCErrorError> {
        let SetDefaultModelParams {
            model,
            reasoning_effort,
        } = params;
        let effort_str = reasoning_effort.map(|effort| effort.to_string());

        let overrides: [(&[&str], Option<&str>); 2] = [
            (&[CONFIG_KEY_MODEL], model.as_deref()),
            (&[CONFIG_KEY_EFFORT], effort_str.as_deref()),
        ];

        persist_overrides_and_clear_if_none(
            &self.config.codex_home,
            self.config.active_profile.as_deref(),
            &overrides,
        )
        .await
        .map_err(|err| JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message: format!("failed to persist overrides: {err}"),
            data: None,
        })?;

        Ok(SetDefaultModelResponse {})
    }

    async fn exec_one_off_command(&self, request_id: RequestId, params: ExecOneOffCommandParams) {
        match self.exec_one_off_command_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn exec_one_off_command_internal(
        &self,
        params: ExecOneOffCommandParams,
    ) -> Result<ExecArbitraryCommandResponse, JSONRPCErrorError> {
        tracing::debug!("ExecOneOffCommand params: {params:?}");

        if params.command.is_empty() {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "command must not be empty".to_string(),
                data: None,
            });
        }

        let cwd = params.cwd.unwrap_or_else(|| self.config.cwd.clone());
        let env = create_env(&self.config.shell_environment_policy);
        let timeout_ms = params.timeout_ms;
        let exec_params = ExecParams {
            command: params.command,
            cwd,
            timeout_ms,
            env,
            with_escalated_permissions: None,
            justification: None,
        };

        let effective_policy = params
            .sandbox_policy
            .unwrap_or_else(|| self.config.sandbox_policy.clone());

        let sandbox_type = match &effective_policy {
            codex_core::protocol::SandboxPolicy::DangerFullAccess => {
                codex_core::exec::SandboxType::None
            }
            _ => get_platform_sandbox().unwrap_or(codex_core::exec::SandboxType::None),
        };
        tracing::debug!("Sandbox type: {sandbox_type:?}");
        let codex_linux_sandbox_exe = self.codex_linux_sandbox_exe.clone();

        codex_core::exec::process_exec_tool_call(
            exec_params,
            sandbox_type,
            &effective_policy,
            &codex_linux_sandbox_exe,
            None,
        )
        .await
        .map(|output| ExecArbitraryCommandResponse {
            exit_code: output.exit_code,
            stdout: output.stdout.text,
            stderr: output.stderr.text,
        })
        .map_err(|err| JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message: format!("exec failed: {err}"),
            data: None,
        })
    }

    async fn process_new_conversation(&self, request_id: RequestId, params: NewConversationParams) {
        match self.new_conversation_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    pub(crate) async fn new_conversation_internal(
        &self,
        params: NewConversationParams,
    ) -> Result<NewConversationResponse, JSONRPCErrorError> {
        let config = derive_config_from_params(params, self.codex_linux_sandbox_exe.clone())
            .map_err(|err| JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("error deriving config: {err}"),
                data: None,
            })?;

        match self.conversation_manager.new_conversation(config).await {
            Ok(NewConversation {
                conversation_id,
                session_configured,
                ..
            }) => Ok(NewConversationResponse {
                conversation_id,
                model: session_configured.model,
                reasoning_effort: session_configured.reasoning_effort,
                rollout_path: session_configured.rollout_path,
            }),
            Err(err) => Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("error creating conversation: {err}"),
                data: None,
            }),
        }
    }

    async fn handle_list_conversations(
        &self,
        request_id: RequestId,
        params: ListConversationsParams,
    ) {
        match self.list_conversations_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn list_conversations_internal(
        &self,
        params: ListConversationsParams,
    ) -> Result<ListConversationsResponse, JSONRPCErrorError> {
        let page_size = params.page_size.unwrap_or(25);
        // Decode the optional cursor string to a Cursor via serde (Cursor implements Deserialize from string)
        let cursor_obj: Option<RolloutCursor> = match params.cursor {
            Some(s) => serde_json::from_str::<RolloutCursor>(&format!("\"{s}\"")).ok(),
            None => None,
        };
        let cursor_ref = cursor_obj.as_ref();

        let page =
            RolloutRecorder::list_conversations(&self.config.codex_home, page_size, cursor_ref)
                .await
                .map_err(|err| JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to list conversations: {err}"),
                    data: None,
                })?;

        let items = page
            .items
            .into_iter()
            .filter_map(|it| extract_conversation_summary(it.path, &it.head))
            .collect();

        // Encode next_cursor as a plain string
        let next_cursor = match page.next_cursor {
            Some(c) => match serde_json::to_value(&c) {
                Ok(serde_json::Value::String(s)) => Some(s),
                _ => None,
            },
            None => None,
        };

        Ok(ListConversationsResponse { items, next_cursor })
    }

    async fn handle_resume_conversation(
        &self,
        request_id: RequestId,
        params: ResumeConversationParams,
    ) {
        match self.resume_conversation_internal(params).await {
            Ok((event, response)) => {
                if let Some(event) = event {
                    self.outgoing.send_event_as_notification(&event, None).await;
                }
                self.outgoing.send_response(request_id, response).await;
            }
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn resume_conversation_internal(
        &self,
        params: ResumeConversationParams,
    ) -> Result<(Option<Event>, ResumeConversationResponse), JSONRPCErrorError> {
        // Derive a Config using the same logic as new conversation, honoring overrides if provided.
        let config = match params.overrides {
            Some(overrides) => {
                derive_config_from_params(overrides, self.codex_linux_sandbox_exe.clone())
            }
            None => Ok(self.config.as_ref().clone()),
        }
        .map_err(|err| JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message: format!("error deriving config: {err}"),
            data: None,
        })?;

        match self
            .conversation_manager
            .resume_conversation_from_rollout(
                config,
                params.path.clone(),
                self.auth_manager.clone(),
            )
            .await
        {
            Ok(NewConversation {
                conversation_id,
                session_configured,
                ..
            }) => {
                let event = Event {
                    id: "".to_string(),
                    msg: EventMsg::SessionConfigured(session_configured.clone()),
                };
                let initial_messages = session_configured.initial_messages.map(|msgs| {
                    msgs.into_iter()
                        .filter(|event| {
                            // Don't send non-plain user messages (like user instructions
                            // or environment context) back so they don't get rendered.
                            if let EventMsg::UserMessage(user_message) = event {
                                return matches!(user_message.kind, Some(InputMessageKind::Plain));
                            }
                            true
                        })
                        .collect()
                });

                let response = ResumeConversationResponse {
                    conversation_id,
                    model: session_configured.model.clone(),
                    initial_messages,
                };
                Ok((Some(event), response))
            }
            Err(err) => Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("error resuming conversation: {err}"),
                data: None,
            }),
        }
    }

    async fn archive_conversation(&self, request_id: RequestId, params: ArchiveConversationParams) {
        match self.archive_conversation_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn archive_conversation_internal(
        &self,
        params: ArchiveConversationParams,
    ) -> Result<ArchiveConversationResponse, JSONRPCErrorError> {
        let ArchiveConversationParams {
            conversation_id,
            rollout_path,
        } = params;

        // Verify that the rollout path is in the sessions directory or else
        // a malicious client could specify an arbitrary path.
        let rollout_folder = self.config.codex_home.join(codex_core::SESSIONS_SUBDIR);
        let canonical_rollout_path = tokio::fs::canonicalize(&rollout_path).await;
        let canonical_rollout_path = if let Ok(path) = canonical_rollout_path
            && path.starts_with(&rollout_folder)
        {
            path
        } else {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "rollout path `{}` must be in sessions directory",
                    rollout_path.display()
                ),
                data: None,
            });
        };

        let required_suffix = format!("{conversation_id}.jsonl");
        let Some(file_name) = canonical_rollout_path.file_name().map(OsStr::to_owned) else {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "rollout path `{}` missing file name",
                    rollout_path.display()
                ),
                data: None,
            });
        };

        if !file_name
            .to_string_lossy()
            .ends_with(required_suffix.as_str())
        {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "rollout path `{}` does not match conversation id {conversation_id}",
                    rollout_path.display()
                ),
                data: None,
            });
        }

        let removed_conversation = self
            .conversation_manager
            .remove_conversation(&conversation_id)
            .await;
        if let Some(conversation) = removed_conversation {
            info!("conversation {conversation_id} was active; shutting down");
            let conversation_clone = conversation.clone();
            let notify = Arc::new(tokio::sync::Notify::new());
            let notify_clone = notify.clone();

            // Establish the listener for ShutdownComplete before submitting
            // Shutdown so it is not missed.
            let is_shutdown = tokio::spawn(async move {
                loop {
                    select! {
                        _ = notify_clone.notified() => {
                            break;
                        }
                        event = conversation_clone.next_event() => {
                            if let Ok(event) = event && matches!(event.msg, EventMsg::ShutdownComplete) {
                                break;
                            }
                        }
                    }
                }
            });

            // Request shutdown.
            match conversation.submit(Op::Shutdown).await {
                Ok(_) => {
                    // Successfully submitted Shutdown; wait before proceeding.
                    select! {
                        _ = is_shutdown => {
                            // Normal shutdown: proceed with archive.
                        }
                        _ = tokio::time::sleep(Duration::from_secs(10)) => {
                            warn!("conversation {conversation_id} shutdown timed out; proceeding with archive");
                            notify.notify_one();
                        }
                    }
                }
                Err(err) => {
                    error!("failed to submit Shutdown to conversation {conversation_id}: {err}");
                    notify.notify_one();
                    // Perhaps we lost a shutdown race, so let's continue to
                    // clean up the .jsonl file.
                }
            }
        }

        // Move the .jsonl file to the archived sessions subdir.
        let result: std::io::Result<()> = async {
            let archive_folder = self
                .config
                .codex_home
                .join(codex_core::ARCHIVED_SESSIONS_SUBDIR);
            tokio::fs::create_dir_all(&archive_folder).await?;
            tokio::fs::rename(&canonical_rollout_path, &archive_folder.join(&file_name)).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => Ok(ArchiveConversationResponse {}),
            Err(err) => Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to archive conversation: {err}"),
                data: None,
            }),
        }
    }

    async fn send_user_message(&self, request_id: RequestId, params: SendUserMessageParams) {
        match self.send_user_message_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn send_user_message_internal(
        &self,
        params: SendUserMessageParams,
    ) -> Result<SendUserMessageResponse, JSONRPCErrorError> {
        let SendUserMessageParams {
            conversation_id,
            items,
        } = params;
        let conversation = self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
            .map_err(|_| JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("conversation not found: {conversation_id}"),
                data: None,
            })?;

        let mapped_items: Vec<CoreInputItem> = items
            .into_iter()
            .map(|item| match item {
                WireInputItem::Text { text } => CoreInputItem::Text { text },
                WireInputItem::Image { image_url } => CoreInputItem::Image { image_url },
                WireInputItem::LocalImage { path } => CoreInputItem::LocalImage { path },
            })
            .collect();

        let _ = conversation
            .submit(Op::UserInput {
                items: mapped_items,
            })
            .await;

        Ok(SendUserMessageResponse {})
    }

    async fn send_user_turn(&self, request_id: RequestId, params: SendUserTurnParams) {
        match self.send_user_turn_internal(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn send_user_turn_internal(
        &self,
        params: SendUserTurnParams,
    ) -> Result<SendUserTurnResponse, JSONRPCErrorError> {
        let SendUserTurnParams {
            conversation_id,
            items,
            cwd,
            approval_policy,
            sandbox_policy,
            model,
            effort,
            summary,
        } = params;

        let conversation = self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
            .map_err(|_| JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("conversation not found: {conversation_id}"),
                data: None,
            })?;

        let mapped_items: Vec<CoreInputItem> = items
            .into_iter()
            .map(|item| match item {
                WireInputItem::Text { text } => CoreInputItem::Text { text },
                WireInputItem::Image { image_url } => CoreInputItem::Image { image_url },
                WireInputItem::LocalImage { path } => CoreInputItem::LocalImage { path },
            })
            .collect();

        let _ = conversation
            .submit(Op::UserTurn {
                items: mapped_items,
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
            })
            .await;

        Ok(SendUserTurnResponse {})
    }

    async fn interrupt_conversation(
        &mut self,
        request_id: RequestId,
        params: InterruptConversationParams,
    ) {
        let InterruptConversationParams { conversation_id } = params;
        if let Err(err) = self
            .schedule_interrupt(
                conversation_id,
                PendingInterrupt::JsonRpc(request_id.clone()),
            )
            .await
        {
            self.outgoing.send_error(request_id, err).await;
        }
    }

    pub(crate) async fn schedule_interrupt(
        &self,
        conversation_id: ConversationId,
        pending: PendingInterrupt,
    ) -> Result<(), JSONRPCErrorError> {
        let conversation = self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
            .map_err(|_| JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("conversation not found: {conversation_id}"),
                data: None,
            })?;

        {
            let mut map = self.pending_interrupts.lock().await;
            map.entry(conversation_id).or_default().push(pending);
        }

        let _ = conversation.submit(Op::Interrupt).await;
        Ok(())
    }

    async fn add_conversation_listener(
        &mut self,
        request_id: RequestId,
        params: AddConversationListenerParams,
    ) {
        let AddConversationListenerParams { conversation_id } = params;
        let Ok(conversation) = self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
        else {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("conversation not found: {conversation_id}"),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        };

        let subscription_id = Uuid::new_v4();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        self.conversation_listeners
            .insert(subscription_id, cancel_tx);
        let outgoing_for_task = self.outgoing.clone();
        let pending_interrupts = self.pending_interrupts.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => {
                        // User has unsubscribed, so exit this task.
                        break;
                    }
                    event = conversation.next_event() => {
                        let event = match event {
                            Ok(event) => event,
                            Err(err) => {
                                tracing::warn!("conversation.next_event() failed with: {err}");
                                break;
                            }
                        };

                        // For now, we send a notification for every event,
                        // JSON-serializing the `Event` as-is, but these should
                        // be migrated to be variants of `ServerNotification`
                        // instead.
                        let method = format!("codex/event/{}", event.msg);
                        let mut params = match serde_json::to_value(event.clone()) {
                            Ok(serde_json::Value::Object(map)) => map,
                            Ok(_) => {
                                error!("event did not serialize to an object");
                                continue;
                            }
                            Err(err) => {
                                error!("failed to serialize event: {err}");
                                continue;
                            }
                        };
                        params.insert("conversationId".to_string(), conversation_id.to_string().into());

                        outgoing_for_task.send_notification(OutgoingNotification {
                            method,
                            params: Some(params.into()),
                        })
                        .await;

                        apply_bespoke_event_handling(event.clone(), conversation_id, conversation.clone(), outgoing_for_task.clone(), pending_interrupts.clone()).await;
                    }
                }
            }
        });
        let response = AddConversationSubscriptionResponse { subscription_id };
        self.outgoing.send_response(request_id, response).await;
    }

    async fn remove_conversation_listener(
        &mut self,
        request_id: RequestId,
        params: RemoveConversationListenerParams,
    ) {
        let RemoveConversationListenerParams { subscription_id } = params;
        match self.conversation_listeners.remove(&subscription_id) {
            Some(sender) => {
                // Signal the spawned task to exit and acknowledge.
                let _ = sender.send(());
                let response = RemoveConversationSubscriptionResponse {};
                self.outgoing.send_response(request_id, response).await;
            }
            None => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("subscription not found: {subscription_id}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    async fn git_diff_to_origin(&self, request_id: RequestId, cwd: PathBuf) {
        match self.git_diff_to_origin_internal(cwd).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(err) => self.outgoing.send_error(request_id, err).await,
        }
    }

    pub(crate) async fn git_diff_to_origin_internal(
        &self,
        cwd: PathBuf,
    ) -> Result<GitDiffToRemoteResponse, JSONRPCErrorError> {
        match git_diff_to_remote(&cwd).await {
            Some(value) => Ok(GitDiffToRemoteResponse {
                sha: value.sha,
                diff: value.diff,
            }),
            None => Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("failed to compute git diff to remote for cwd: {cwd:?}"),
                data: None,
            }),
        }
    }
}

async fn apply_bespoke_event_handling(
    event: Event,
    conversation_id: ConversationId,
    conversation: Arc<CodexConversation>,
    outgoing: Arc<OutgoingMessageSender>,
    pending_interrupts: Arc<Mutex<HashMap<ConversationId, Vec<PendingInterrupt>>>>,
) {
    let Event { id: event_id, msg } = event;
    match msg {
        EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            changes,
            reason,
            grant_root,
        }) => {
            let params = ApplyPatchApprovalParams {
                conversation_id,
                call_id,
                file_changes: changes,
                reason,
                grant_root,
            };
            let value = serde_json::to_value(&params).unwrap_or_default();
            let rx = outgoing
                .send_request(APPLY_PATCH_APPROVAL_METHOD, Some(value))
                .await;
            // TODO(mbolin): Enforce a timeout so this task does not live indefinitely?
            tokio::spawn(async move {
                on_patch_approval_response(event_id, rx, conversation).await;
            });
        }
        EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            call_id,
            command,
            cwd,
            reason,
        }) => {
            let params = ExecCommandApprovalParams {
                conversation_id,
                call_id,
                command,
                cwd,
                reason,
            };
            let value = serde_json::to_value(&params).unwrap_or_default();
            let rx = outgoing
                .send_request(EXEC_COMMAND_APPROVAL_METHOD, Some(value))
                .await;

            // TODO(mbolin): Enforce a timeout so this task does not live indefinitely?
            tokio::spawn(async move {
                on_exec_approval_response(event_id, rx, conversation).await;
            });
        }
        // If this is a TurnAborted, reply to any pending interrupt requests.
        EventMsg::TurnAborted(turn_aborted_event) => {
            let pending = {
                let mut map = pending_interrupts.lock().await;
                map.remove(&conversation_id).unwrap_or_default()
            };
            if !pending.is_empty() {
                let response = InterruptConversationResponse {
                    abort_reason: turn_aborted_event.reason,
                };
                let structured = serde_json::to_value(&response).unwrap_or_else(|_| json!({}));
                for rid in pending {
                    match rid {
                        PendingInterrupt::JsonRpc(rid) => {
                            outgoing.send_response(rid, response.clone()).await;
                        }
                        PendingInterrupt::Tool(rid) => {
                            let result = CallToolResult {
                                content: vec![ContentBlock::TextContent(TextContent {
                                    r#type: "text".to_string(),
                                    text: format!(
                                        "Conversation interrupted with reason: {:?}",
                                        response.abort_reason
                                    ),
                                    annotations: None,
                                })],
                                is_error: None,
                                structured_content: Some(structured.clone()),
                            };
                            outgoing.send_response(rid, result).await;
                        }
                    }
                }
            }
        }

        _ => {}
    }
}

fn derive_config_from_params(
    params: NewConversationParams,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> std::io::Result<Config> {
    let NewConversationParams {
        model,
        profile,
        cwd,
        approval_policy,
        sandbox: sandbox_mode,
        config: cli_overrides,
        base_instructions,
        include_plan_tool,
        include_apply_patch_tool,
    } = params;
    let overrides = ConfigOverrides {
        model,
        review_model: None,
        config_profile: profile,
        cwd: cwd.map(PathBuf::from),
        approval_policy,
        sandbox_mode,
        model_provider: None,
        codex_linux_sandbox_exe,
        base_instructions,
        include_plan_tool,
        include_apply_patch_tool,
        include_view_image_tool: None,
        show_raw_agent_reasoning: None,
        tools_web_search_request: None,
    };

    let cli_overrides = cli_overrides
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, json_to_toml(v)))
        .collect();

    Config::load_with_cli_overrides(cli_overrides, overrides)
}

async fn on_patch_approval_response(
    event_id: String,
    receiver: oneshot::Receiver<mcp_types::Result>,
    codex: Arc<CodexConversation>,
) {
    let response = receiver.await;
    let value = match response {
        Ok(value) => value,
        Err(err) => {
            error!("request failed: {err:?}");
            if let Err(submit_err) = codex
                .submit(Op::PatchApproval {
                    id: event_id.clone(),
                    decision: ReviewDecision::Denied,
                })
                .await
            {
                error!("failed to submit denied PatchApproval after request failure: {submit_err}");
            }
            return;
        }
    };

    let response =
        serde_json::from_value::<ApplyPatchApprovalResponse>(value).unwrap_or_else(|err| {
            error!("failed to deserialize ApplyPatchApprovalResponse: {err}");
            ApplyPatchApprovalResponse {
                decision: ReviewDecision::Denied,
            }
        });

    if let Err(err) = codex
        .submit(Op::PatchApproval {
            id: event_id,
            decision: response.decision,
        })
        .await
    {
        error!("failed to submit PatchApproval: {err}");
    }
}

async fn on_exec_approval_response(
    event_id: String,
    receiver: oneshot::Receiver<mcp_types::Result>,
    conversation: Arc<CodexConversation>,
) {
    let response = receiver.await;
    let value = match response {
        Ok(value) => value,
        Err(err) => {
            error!("request failed: {err:?}");
            return;
        }
    };

    // Try to deserialize `value` and then make the appropriate call to `codex`.
    let response =
        serde_json::from_value::<ExecCommandApprovalResponse>(value).unwrap_or_else(|err| {
            error!("failed to deserialize ExecCommandApprovalResponse: {err}");
            // If we cannot deserialize the response, we deny the request to be
            // conservative.
            ExecCommandApprovalResponse {
                decision: ReviewDecision::Denied,
            }
        });

    if let Err(err) = conversation
        .submit(Op::ExecApproval {
            id: event_id,
            decision: response.decision,
        })
        .await
    {
        error!("failed to submit ExecApproval: {err}");
    }
}

fn extract_conversation_summary(
    path: PathBuf,
    head: &[serde_json::Value],
) -> Option<ConversationSummary> {
    let session_meta = match head.first() {
        Some(first_line) => serde_json::from_value::<SessionMeta>(first_line.clone()).ok()?,
        None => return None,
    };

    let preview = head
        .iter()
        .filter_map(|value| serde_json::from_value::<ResponseItem>(value.clone()).ok())
        .find_map(|item| match item {
            ResponseItem::Message { content, .. } => {
                content.into_iter().find_map(|content| match content {
                    ContentItem::InputText { text } => {
                        match InputMessageKind::from(("user", &text)) {
                            InputMessageKind::Plain => Some(text),
                            _ => None,
                        }
                    }
                    _ => None,
                })
            }
            _ => None,
        })?;

    let preview = match preview.find(USER_MESSAGE_BEGIN) {
        Some(idx) => preview[idx + USER_MESSAGE_BEGIN.len()..].trim(),
        None => preview.as_str(),
    };

    let timestamp = if session_meta.timestamp.is_empty() {
        None
    } else {
        Some(session_meta.timestamp.clone())
    };

    Some(ConversationSummary {
        conversation_id: session_meta.id,
        timestamp,
        path,
        preview: preview.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn extract_conversation_summary_prefers_plain_user_messages() {
        let conversation_id =
            ConversationId::from_string("3f941c35-29b3-493b-b0a4-e25800d9aeb0").unwrap();
        let timestamp = Some("2025-09-05T16:53:11.850Z".to_string());
        let path = PathBuf::from("rollout.jsonl");

        let head = vec![
            json!({
                "id": conversation_id.to_string(),
                "timestamp": timestamp,
                "cwd": "/",
                "originator": "codex",
                "cli_version": "0.0.0",
                "instructions": null
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<user_instructions>\n<AGENTS.md contents>\n</user_instructions>".to_string(),
                }],
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("<prior context> {USER_MESSAGE_BEGIN}Count to 5"),
                }],
            }),
        ];

        let summary = extract_conversation_summary(path.clone(), &head).expect("summary");

        assert_eq!(summary.conversation_id, conversation_id);
        assert_eq!(
            summary.timestamp,
            Some("2025-09-05T16:53:11.850Z".to_string())
        );
        assert_eq!(summary.path, path);
        assert_eq!(summary.preview, "Count to 5");
    }
}
