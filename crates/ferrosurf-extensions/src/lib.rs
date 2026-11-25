//! FerroSurf extension system.
//!
//! Servo has no WebExtensions API, so extensions are built on two embedder
//! primitives: injected user scripts (see [`ExtensionManager::user_scripts`])
//! and network interception (see [`Blocker`]).

pub mod blocker;
pub mod extension;
pub mod manager;
pub mod manifest;
pub mod match_pattern;
mod userscript;

pub use blocker::{
    Blocker, Decision, DnrAction, DnrActionType, DnrCondition, DnrDomainType, DnrRedirect,
    DnrResourceType, DnrRule, DnrTransform, RequestInfo, ResourceType,
};
pub use extension::{Extension, InstallSource};
pub use manager::ExtensionManager;
pub use manifest::{
    ContentScriptDecl, DnrDecl, FerroSurfExt, Manifest, RuleResource, RunAt, UrlOverrides,
    UserStyleDecl,
};
pub use match_pattern::{MatchPattern, PatternError};
