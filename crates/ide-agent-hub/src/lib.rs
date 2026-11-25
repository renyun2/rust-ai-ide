pub mod acp;
pub mod browser_bridge;
pub mod cli_discovery;
pub mod cli_runner;
pub mod http_fallback;
pub mod hub;
pub mod tools;
pub mod types;

pub use acp::AcpProtocol;
pub use browser_bridge::ensure_cursor_persistent_chrome_mcp;
pub use cli_discovery::{
    cli_install_spec, detect_node_and_npm, discover_local_cli_registrations,
    install_agent_cli_blocking, normalize_cursor_base_model, parse_cli_rewind_capabilities,
    probe_cli_rewind_capabilities, resolve_agy_model_and_effort, resolve_cursor_model_id,
    resolve_native_agy_binary, suppress_agy_background_updater_window, CliInstallError,
    CliInstallSpec, NodeEnvironmentStatus, NODEJS_DOWNLOAD_URL,
};
pub use cli_runner::{detect_workspace_file_references, parse_path_and_line_token};
pub use hub::AgentHub;
pub use tools::{extract_enclosing_block, extract_first_fenced_code_block, load_workspace_agent_rules};
pub use types::{
    AgentAttachedItem, AgentChatMessage, AgentProviderKind, AgentRegistration, AgentSession,
    QueuedAgentPrompt,
    AgentWakeupCallback, AgentWorkbenchMode, AiApiConfig, AiApiFormat, IdeCodeContext,
    InlineEditSession, PastedCodeSnippet, PersistedAgentHistory, ProposedFilePatch,
    ReasoningEffort, SessionFileChange, StreamingAgentProgress, TurnFileChange, TurnSyncAnchor,
    WorkspaceCheckpoint,
};
