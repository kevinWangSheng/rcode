mod merge;
mod paths;
pub mod project;
pub mod settings;

pub use paths::ConfigPaths;
pub use project::ProjectContext;
pub use settings::{
    discover_sources, expand_model_alias, load_settings, load_settings_with_override,
    merge_sources, resolve_model, PermissionsConfig, ResolvedConfig, Settings, SettingsSources,
    SettingsSourcesEnabled,
};
