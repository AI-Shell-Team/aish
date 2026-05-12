use crate::profile::HostProfile;

const HOSTS_DIR_NAME: &str = "hosts";

fn hosts_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("aish")
        .join(HOSTS_DIR_NAME)
}

pub fn sanitize_host_key(host_key: &str) -> String {
    host_key.replace(['/', '\\', ':'], "_").replace('\0', "")
}

fn profile_path(host_key: &str) -> std::path::PathBuf {
    hosts_dir().join(format!("{}.yaml", sanitize_host_key(host_key)))
}

pub fn load_profile(host_key: &str) -> Option<HostProfile> {
    let path = profile_path(host_key);
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&content).ok()
}

pub fn save_profile(profile: &HostProfile) -> std::io::Result<()> {
    let dir = hosts_dir();
    std::fs::create_dir_all(&dir)?;
    let path = profile_path(&profile.host_key);
    let content = serde_yaml::to_string(profile)
        .map_err(std::io::Error::other)?;
    std::fs::write(path, content)
}

pub fn get_or_create_profile(host_key: &str) -> HostProfile {
    load_profile(host_key).unwrap_or_else(|| HostProfile::new(host_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::HostProfile;

    #[test]
    fn test_sanitize_host_key() {
        assert_eq!(sanitize_host_key("root@192.168.1.100"), "root@192.168.1.100");
        assert_eq!(sanitize_host_key("user@example.com"), "user@example.com");
    }

    #[test]
    fn test_load_nonexistent_returns_none() {
        let result = load_profile("nonexistent_host_99999");
        assert!(result.is_none());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut profile = HostProfile::new("test@roundtrip.example.com");
        profile.system.os = "TestOS 1.0".to_string();
        profile.add_note("test note".to_string());

        let path = dir.path().join("test@roundtrip.example.com.yaml");
        let content = serde_yaml::to_string(&profile).unwrap();
        std::fs::write(&path, &content).unwrap();

        let loaded: HostProfile =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.host_key, "test@roundtrip.example.com");
        assert_eq!(loaded.system.os, "TestOS 1.0");
        assert_eq!(loaded.notes.len(), 1);
    }
}
