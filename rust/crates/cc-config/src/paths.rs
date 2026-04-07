use std::path::PathBuf;

/// Standard paths used by Claude Code.
pub struct ConfigPaths;

impl ConfigPaths {
    /// `~/.claude/`
    pub fn global_config_dir() -> PathBuf {
        dirs::home_dir()
            .expect("cannot determine home directory")
            .join(".claude")
    }

    /// `~/.claude/settings.json`
    pub fn global_settings() -> PathBuf {
        Self::global_config_dir().join("settings.json")
    }

    /// `.claude/settings.json` relative to `cwd`
    pub fn project_settings(cwd: &std::path::Path) -> PathBuf {
        cwd.join(".claude").join("settings.json")
    }

    /// `.claude/settings.local.json` relative to `cwd`
    pub fn local_settings(cwd: &std::path::Path) -> PathBuf {
        cwd.join(".claude").join("settings.local.json")
    }

    /// `~/.claude/memory/`
    pub fn memory_dir() -> PathBuf {
        Self::global_config_dir().join("memory")
    }

    /// `~/.claude/sessions/`
    pub fn sessions_dir() -> PathBuf {
        Self::global_config_dir().join("sessions")
    }

    /// `~/.claude/skills/`
    pub fn skills_dir() -> PathBuf {
        Self::global_config_dir().join("skills")
    }

    /// `~/.claude/history.jsonl`
    pub fn history_file() -> PathBuf {
        Self::global_config_dir().join("history.jsonl")
    }
}
