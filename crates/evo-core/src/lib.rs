//! evo-core — agent loop, session, prompt, summary protocol, skill, memory,
//! reflection. Phase 1 + Phase 2.

pub mod channel;
pub mod channel_config;
pub mod channel_router;
pub mod compression;
pub mod distillation;
pub mod local_pipe;
pub mod memory;
pub mod prompt;
pub mod reflection;
pub mod runtime;
pub mod session;
pub mod skill;
pub mod skill_tree;
pub mod summary;
pub mod discord;
pub mod slack;
pub mod telegram;

pub use compression::{compress_if_due, CompressionConfig};
pub use distillation::{
    build_distillation_prompt, parse_distilled_skill, skill_from_reflection_quick, DistillCtx,
};
pub use memory::{Memory, MemoryLayer, MemoryRecord};
pub use prompt::{build_system_prompt, PromptCtx};
pub use reflection::{
    build_reflection_prompt, parse_reflection, ReflectionCtx, ReflectionRecord, SkillUpdateDecision,
};
pub use runtime::{ConversationRuntime, RunOutcome, RuntimeError};
pub use session::{Session, TaskRecord, TurnRecord};
pub use skill::{FailurePattern, Skill, SkillKind, SkillState, SkillStats, SkillStep};
pub use skill_tree::{SkillTree, SkillTreeNode};
pub use summary::{extract_summary, SummaryParser};
