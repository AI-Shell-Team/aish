use std::path::Path;

pub const INSTALL_CHANNEL_ENV: &str = "AISH_INSTALL_CHANNEL";
pub const PIP_PACKAGE_NAME_ENV: &str = "AISH_PIP_PACKAGE_NAME";
pub const PYTHON_EXECUTABLE_ENV: &str = "AISH_PYTHON_EXECUTABLE";

const PIP_CHANNEL: &str = "pip";
const DEFAULT_PIP_PACKAGE_NAME: &str = "aish-rust";
const PIP_PACKAGE_DIRS: &[&str] = &["aish_rust", "ai_sh"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipChannelContext {
    pub package_name: String,
    pub python_executable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChannel {
    Pip(PipChannelContext),
}

pub fn current_install_channel() -> Option<InstallChannel> {
    resolve_install_channel_with(|name| std::env::var(name).ok(), current_exe_path())
}

fn current_exe_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(ToOwned::to_owned))
}

fn resolve_install_channel_with<F>(env_get: F, exe_path: Option<String>) -> Option<InstallChannel>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(PIP_CHANNEL) = env_get(INSTALL_CHANNEL_ENV).as_deref() {
        return Some(InstallChannel::Pip(PipChannelContext {
            package_name: env_get(PIP_PACKAGE_NAME_ENV)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_PIP_PACKAGE_NAME.to_string()),
            python_executable: env_get(PYTHON_EXECUTABLE_ENV)
                .filter(|value| !value.trim().is_empty()),
        }));
    }

    if exe_path.as_deref().is_some_and(is_pip_binary_path) {
        return Some(InstallChannel::Pip(PipChannelContext {
            package_name: DEFAULT_PIP_PACKAGE_NAME.to_string(),
            python_executable: env_get(PYTHON_EXECUTABLE_ENV)
                .filter(|value| !value.trim().is_empty()),
        }));
    }

    None
}

fn is_pip_binary_path(path: &str) -> bool {
    let path = Path::new(path);
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if file_name != "aish" {
        return false;
    }

    let Some(parent_name) = path
        .parent()
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
    else {
        return false;
    };
    if parent_name != "bin" {
        return false;
    }

    let Some(package_name) = path
        .parent()
        .and_then(|value| value.parent())
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
    else {
        return false;
    };

    PIP_PACKAGE_DIRS.contains(&package_name)
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_install_channel_with, InstallChannel, PipChannelContext, INSTALL_CHANNEL_ENV,
        PIP_PACKAGE_NAME_ENV, PYTHON_EXECUTABLE_ENV,
    };

    #[test]
    fn test_resolve_pip_channel_from_env() {
        let channel = resolve_install_channel_with(
            |name| match name {
                INSTALL_CHANNEL_ENV => Some("pip".to_string()),
                PIP_PACKAGE_NAME_ENV => Some("aish-rust".to_string()),
                PYTHON_EXECUTABLE_ENV => Some("/tmp/venv/bin/python".to_string()),
                _ => None,
            },
            None,
        );

        assert_eq!(
            channel,
            Some(InstallChannel::Pip(PipChannelContext {
                package_name: "aish-rust".to_string(),
                python_executable: Some("/tmp/venv/bin/python".to_string()),
            }))
        );
    }

    #[test]
    fn test_resolve_pip_channel_from_binary_path() {
        let channel = resolve_install_channel_with(
            |_| None,
            Some("/tmp/venv/lib/python3.12/site-packages/aish_rust/bin/aish".to_string()),
        );

        assert_eq!(
            channel,
            Some(InstallChannel::Pip(PipChannelContext {
                package_name: "aish-rust".to_string(),
                python_executable: None,
            }))
        );
    }

    #[test]
    fn test_resolve_pip_channel_from_legacy_binary_path() {
        let channel = resolve_install_channel_with(
            |_| None,
            Some("/tmp/venv/lib/python3.12/site-packages/ai_sh/bin/aish".to_string()),
        );

        assert_eq!(
            channel,
            Some(InstallChannel::Pip(PipChannelContext {
                package_name: "aish-rust".to_string(),
                python_executable: None,
            }))
        );
    }
}
