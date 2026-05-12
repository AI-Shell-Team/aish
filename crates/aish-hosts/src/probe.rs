use crate::profile::SystemInfo;

const PROBE_MARKER: &str = "___AISH_PROBE___";

pub fn probe_command() -> String {
    // Use printf to construct the marker so the literal marker text never
    // appears in the command itself.  Without this, the remote shell echoes
    // the command (including the markers), causing the parser to see double
    // the expected marker count.
    //
    // The `which` check is intentionally omitted here — it is slow on
    // some systems and the AI can discover tools on demand.  Only the
    // 5 essential sections are probed: os, kernel, shell, user/home, locale.
    // This reduces the probe from 6 markers to 5, making it faster.
    "M=$(printf '\\137\\137\\137AISH_PROBE\\137\\137\\137'); \
         echo \"$M\"; \
         cat /etc/os-release 2>/dev/null | head -5; \
         echo \"$M\"; \
         uname -rm 2>/dev/null; \
         echo \"$M\"; \
         echo \"$SHELL\"; \
         echo \"$M\"; \
         whoami; echo \"$HOME\"; \
         echo \"$M\"; \
         echo \"$LANG\""
        .to_string()
}

pub fn probe_marker() -> &'static str {
    PROBE_MARKER
}

pub fn parse_probe_output(sections: &[String]) -> SystemInfo {
    let mut info = SystemInfo::default();

    // Section layout (after removing section[0] which is the command echo):
    //   [0] = os-release   [1] = kernel   [2] = shell
    //   [3] = user/home     [4] = locale
    if let Some(s) = sections.first() {
        info.os = parse_os_release(s);
        info.package_manager = infer_package_manager(s);
    }
    if let Some(s) = sections.get(1) {
        info.kernel = s.trim().to_string();
    }
    if let Some(s) = sections.get(2) {
        info.shell = s.trim().to_string();
    }
    if let Some(s) = sections.get(3) {
        // Sections start with \r\n from the marker's trailing newline,
        // producing an empty first line — skip it.
        let lines: Vec<&str> = s
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if let Some(first) = lines.first() {
            info.user = first.to_string();
        }
        if let Some(second) = lines.get(1) {
            info.home = second.to_string();
        }
    }
    if let Some(s) = sections.get(4) {
        info.locale = s
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
    }

    info
}

fn parse_os_release(raw: &str) -> String {
    let mut name = String::new();
    let mut version = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
            return val.trim_matches('"').to_string();
        }
        if let Some(val) = line.strip_prefix("NAME=") {
            name = val.trim_matches('"').to_string();
        }
        if let Some(val) = line.strip_prefix("VERSION=") {
            version = val.trim_matches('"').to_string();
        }
    }
    if !name.is_empty() {
        format!("{} {}", name, version).trim().to_string()
    } else {
        raw.trim().to_string()
    }
}

fn infer_package_manager(os_release: &str) -> String {
    for line in os_release.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("ID=") {
            let id = val.trim_matches('"').to_lowercase();
            return match id.as_str() {
                "debian" | "ubuntu" | "uos" | "deepin" | "linuxmint" | "pop" | "elementary"
                | "kali" | "raspbian" => "apt",
                "rhel" | "centos" | "rocky" | "almalinux" | "ol" | "anolis" => "yum",
                "fedora" => "dnf",
                "arch" | "manjaro" | "endeavouros" | "garuda" => "pacman",
                "sles" | "suse" | "opensuse" => "zypper",
                "alpine" => "apk",
                _ => "",
            }
            .to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_command_uses_printf() {
        let cmd = probe_command();
        // The command must NOT contain the literal marker (it would be echoed)
        assert!(!cmd.contains(PROBE_MARKER));
        // Instead it uses printf to build the marker at runtime
        assert!(cmd.contains("printf"));
        assert!(cmd.contains("\\137"));
    }

    #[test]
    fn test_parse_ubuntu_os_release() {
        let raw = "NAME=\"Ubuntu\"\nVERSION=\"22.04.3 LTS (Jammy Jellyfish)\"\nPRETTY_NAME=\"Ubuntu 22.04.3 LTS\"";
        assert_eq!(parse_os_release(raw), "Ubuntu 22.04.3 LTS");
    }

    #[test]
    fn test_parse_centos_os_release() {
        let raw = "NAME=\"CentOS Linux\"\nVERSION=\"7 (Core)\"";
        assert_eq!(parse_os_release(raw), "CentOS Linux 7 (Core)");
    }

    #[test]
    fn test_parse_probe_output_full() {
        let sections = vec![
            "ID=ubuntu\nPRETTY_NAME=\"Ubuntu 22.04.3 LTS\"".to_string(),
            "5.15.0-91-generic x86_64".to_string(),
            "/bin/bash".to_string(),
            "root\n/root".to_string(),
            "en_US.UTF-8".to_string(),
        ];
        let info = parse_probe_output(&sections);
        assert_eq!(info.os, "Ubuntu 22.04.3 LTS");
        assert_eq!(info.kernel, "5.15.0-91-generic x86_64");
        assert_eq!(info.shell, "/bin/bash");
        assert_eq!(info.user, "root");
        assert_eq!(info.home, "/root");
        assert_eq!(info.package_manager, "apt");
        assert_eq!(info.locale, "en_US.UTF-8");
    }

    #[test]
    fn test_parse_probe_output_empty_sections() {
        let sections: Vec<String> = vec![];
        let info = parse_probe_output(&sections);
        assert!(info.os.is_empty());
    }
}
