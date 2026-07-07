// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::module_inception,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments
)]

pub mod registry;

pub mod agent_tool {
    mod agent_tool;
    mod prompt;

    pub use self::agent_tool::{AgentTool, SkillCallbacks, SpawnFn};
}

pub mod ask_user {
    mod ask_user;
    mod prompt;

    pub use self::ask_user::*;
}

pub mod bash {
    mod bash;
    mod prompt;
    mod read_only;

    pub use self::bash::*;
    pub use self::read_only::{classify, is_read_only, preflight_enforce, ReadOnlyVerdict};
}

pub mod channel_ask_user {
    mod channel_ask_user;
    mod prompt;

    pub use self::channel_ask_user::*;
}

pub mod channel_bash {
    mod channel_bash;
    mod prompt;

    pub use self::channel_bash::*;
}

pub mod final_answer {
    mod final_answer;
    mod prompt;

    pub use self::final_answer::*;
}

pub mod edit_file {
    mod edit_file;
    mod prompt;

    pub use self::edit_file::*;
}

pub mod fs {
    pub use crate::edit_file::EditFileTool;
    pub use crate::read_file::{ReadFileTool, SshReadFileTool};
    pub use crate::write_file::WriteFileTool;
}

pub mod glob_tool {
    mod glob_tool;
    mod prompt;

    pub use self::glob_tool::*;
}

pub mod grep_tool {
    mod grep_tool;
    mod prompt;

    pub use self::grep_tool::*;
}

pub mod host_note {
    mod host_note;
    mod prompt;

    pub use self::host_note::*;
}

pub mod memory_tool {
    mod memory_tool;
    mod prompt;

    pub use self::memory_tool::*;
}

pub mod plan_tool {
    mod enter_plan_mode;
    mod exit_plan_mode;
    mod list_plan_templates;
    mod prompt;

    pub use self::enter_plan_mode::EnterPlanModeTool;
    pub use self::exit_plan_mode::ExitPlanModeTool;
    pub use self::list_plan_templates::ListTemplatesTool;
}

pub mod python {
    mod prompt;
    mod python;

    pub use self::python::*;
}

pub mod read_file {
    mod prompt;
    mod read_file;

    pub use self::read_file::*;
}

pub mod secure_bash {
    mod secure_bash;

    pub use self::secure_bash::*;
}

pub mod skill_tool {
    mod prompt;
    mod skill_tool;

    pub use self::skill_tool::*;
}

pub mod system_diagnose {
    mod prompt;
    mod system_diagnose;

    pub use self::system_diagnose::*;
}

pub mod web_fetch {
    mod preapproved;
    mod prompt;
    mod utils;
    mod web_fetch;

    pub use self::web_fetch::*;
}

pub mod write_file {
    mod prompt;
    mod write_file;

    pub use self::write_file::*;
}

pub use agent_tool::{AgentTool, SpawnFn};
pub use ask_user::AskUserTool;
pub use channel_ask_user::ChannelAskUserTool;
pub use channel_bash::ChannelBashTool;
pub use final_answer::FinalAnswerTool;
pub use fs::{EditFileTool, ReadFileTool, WriteFileTool};
pub use glob_tool::GlobTool;
pub use grep_tool::GrepTool;
pub use host_note::{HostNoteEntry, HostNoteTool};
pub use memory_tool::{MemorySearchResult, MemoryTool};
pub use plan_tool::{EnterPlanModeTool, ExitPlanModeTool, ListTemplatesTool};
pub use python::PythonTool;
pub use registry::ToolRegistry;
pub use secure_bash::SecureBashTool;
pub use skill_tool::{SkillInfo, SkillTool};
pub use system_diagnose::SharedEventCallback;
pub use system_diagnose::SystemDiagnoseTool;
pub use web_fetch::WebFetchTool;
