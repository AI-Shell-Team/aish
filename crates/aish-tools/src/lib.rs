// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
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

pub mod ask_user;
pub mod bash;
pub mod final_answer;
pub mod fs;
pub mod glob_tool;
pub mod grep_tool;
pub mod memory_tool;
pub mod plan_tool;
pub mod python;
pub mod registry;
pub mod secure_bash;
pub mod skill_tool;

pub use ask_user::AskUserTool;
pub use final_answer::FinalAnswerTool;
pub use fs::{EditFileTool, ReadFileTool, WriteFileTool};
pub use glob_tool::GlobTool;
pub use grep_tool::GrepTool;
pub use memory_tool::{MemorySearchResult, MemoryTool};
pub use plan_tool::{EnterPlanModeTool, ExitPlanModeTool, ListTemplatesTool};
pub use python::PythonTool;
pub use registry::ToolRegistry;
pub use secure_bash::SecureBashTool;
pub use skill_tool::{SkillInfo, SkillTool};
