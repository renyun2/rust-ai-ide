pub mod acp;
pub mod cli_discovery;
pub mod cli_runner;
pub mod http_fallback;
pub mod hub;
pub mod tools;
pub mod types;

pub use acp::AcpProtocol;
pub use cli_discovery::{
    discover_local_cli_registrations, normalize_cursor_base_model, resolve_agy_model_and_effort,
    resolve_cursor_model_id,
};
pub use cli_runner::{detect_workspace_file_references, parse_path_and_line_token};
pub use hub::AgentHub;
pub use tools::{extract_enclosing_block, extract_first_fenced_code_block, load_workspace_agent_rules};
pub use types::{
    AgentAttachedItem, AgentChatMessage, AgentProviderKind, AgentRegistration, AgentSession,
    AgentWakeupCallback, AgentWorkbenchMode, AiApiConfig, AiApiFormat, IdeCodeContext,
    InlineEditSession, PastedCodeSnippet, ProposedFilePatch, ReasoningEffort, WorkspaceCheckpoint,
};
