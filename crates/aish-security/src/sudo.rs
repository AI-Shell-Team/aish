use std::ffi::CString;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrippedSudoCommand {
    pub command: String,
    pub sudo_detected: bool,
    pub ok: bool,
    pub user: Option<String>,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoPayloadKind {
    NotSudo,
    Root,
    User { uid: u32, gid: u32 },
}

impl StrippedSudoCommand {
    fn unchanged(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            sudo_detected: false,
            ok: true,
            user: None,
            group: None,
        }
    }

    fn sudo(command: String, ok: bool, user: Option<String>, group: Option<String>) -> Self {
        Self {
            command,
            sudo_detected: true,
            ok,
            user,
            group,
        }
    }

    pub fn payload_kind(&self) -> Result<SudoPayloadKind, String> {
        if !self.sudo_detected {
            return Ok(SudoPayloadKind::NotSudo);
        }
        if !self.ok {
            return Err("sudo_without_command".to_string());
        }

        let Some(user) = self.user.as_deref().filter(|user| !user.is_empty()) else {
            return Ok(SudoPayloadKind::Root);
        };

        let (uid, default_gid) = lookup_user(user)?;
        if uid == 0 {
            return Ok(SudoPayloadKind::Root);
        }

        let gid = match self.group.as_deref().filter(|group| !group.is_empty()) {
            Some(group) => lookup_group(group)?,
            None => default_gid,
        };
        Ok(SudoPayloadKind::User { uid, gid })
    }
}

pub fn strip_sudo_prefix(command: &str) -> StrippedSudoCommand {
    let raw_l = command.trim_start();
    if !raw_l.starts_with("sudo ") && raw_l != "sudo" {
        return StrippedSudoCommand::unchanged(command.to_string());
    }

    let (token, next_index) = read_token(raw_l, 0);
    if token != "sudo" {
        return StrippedSudoCommand::unchanged(command.to_string());
    }

    let mut index = next_index;
    let mut user = None;
    let mut group = None;
    let options_with_value = ["-u", "--user", "-g", "--group", "-h", "-p", "--prompt"];

    loop {
        index = skip_ws(raw_l, index);
        if index >= raw_l.len() {
            return StrippedSudoCommand::sudo(String::new(), false, user, group);
        }

        let (opt, opt_end) = read_token(raw_l, index);
        if opt.is_empty() {
            return StrippedSudoCommand::sudo(String::new(), false, user, group);
        }

        if opt == "--" {
            index = opt_end;
            break;
        }

        if opt.starts_with('-') {
            if options_with_value.contains(&opt.as_str()) {
                index = opt_end;
                let (value, value_end) = read_token(raw_l, index);
                index = value_end;
                match opt.as_str() {
                    "-u" | "--user" => user = Some(value),
                    "-g" | "--group" => group = Some(value),
                    _ => {}
                }
                continue;
            }

            if opt.starts_with("-u") && opt != "-u" {
                user = Some(opt[2..].to_string());
                index = opt_end;
                continue;
            }

            if opt.starts_with("-g") && opt != "-g" {
                group = Some(opt[2..].to_string());
                index = opt_end;
                continue;
            }

            if let Some(value) = opt.strip_prefix("--user=") {
                user = Some(value.to_string());
                index = opt_end;
                continue;
            }

            if let Some(value) = opt.strip_prefix("--group=") {
                group = Some(value.to_string());
                index = opt_end;
                continue;
            }

            if opt.starts_with("--prompt=") {
                index = opt_end;
                continue;
            }

            index = opt_end;
            continue;
        }

        break;
    }

    let stripped = raw_l[index..].trim_start().to_string();
    if stripped.is_empty() {
        return StrippedSudoCommand::sudo(String::new(), false, user, group);
    }

    StrippedSudoCommand::sudo(stripped, true, user, group)
}

fn lookup_user(spec: &str) -> Result<(u32, u32), String> {
    if let Ok(uid) = spec.parse::<u32>() {
        return Ok(match lookup_passwd_by_uid(uid) {
            Some(gid) => (uid, gid),
            None => (uid, uid),
        });
    }

    lookup_passwd_by_name(spec).ok_or_else(|| "sudo_unknown_user".to_string())
}

fn lookup_group(spec: &str) -> Result<u32, String> {
    if let Ok(gid) = spec.parse::<u32>() {
        return Ok(gid);
    }

    lookup_group_by_name(spec).ok_or_else(|| "sudo_unknown_group".to_string())
}

fn lookup_passwd_by_name(name: &str) -> Option<(u32, u32)> {
    let c_name = CString::new(name).ok()?;
    let mut pwd = empty_passwd();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 4096];
    let rc = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut pwd,
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some((pwd.pw_uid, pwd.pw_gid))
}

fn lookup_passwd_by_uid(uid: u32) -> Option<u32> {
    let mut pwd = empty_passwd();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 4096];
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some(pwd.pw_gid)
}

fn lookup_group_by_name(name: &str) -> Option<u32> {
    let c_name = CString::new(name).ok()?;
    let mut grp = libc::group {
        gr_name: std::ptr::null_mut(),
        gr_passwd: std::ptr::null_mut(),
        gr_gid: 0,
        gr_mem: std::ptr::null_mut(),
    };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 4096];
    let rc = unsafe {
        libc::getgrnam_r(
            c_name.as_ptr(),
            &mut grp,
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some(grp.gr_gid)
}

fn empty_passwd() -> libc::passwd {
    libc::passwd {
        pw_name: std::ptr::null_mut(),
        pw_passwd: std::ptr::null_mut(),
        pw_uid: 0,
        pw_gid: 0,
        pw_gecos: std::ptr::null_mut(),
        pw_dir: std::ptr::null_mut(),
        pw_shell: std::ptr::null_mut(),
    }
}

fn skip_ws(s: &str, mut index: usize) -> usize {
    while index < s.len() {
        let ch = s[index..].chars().next().expect("valid char boundary");
        if !ch.is_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn read_token(s: &str, index: usize) -> (String, usize) {
    let mut index = skip_ws(s, index);
    if index >= s.len() {
        return (String::new(), index);
    }

    let mut out = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while index < s.len() {
        let ch = s[index..].chars().next().expect("valid char boundary");
        if !in_single_quote && !in_double_quote && ch.is_whitespace() {
            break;
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            index += ch.len_utf8();
            continue;
        }

        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            index += ch.len_utf8();
            continue;
        }

        if ch == '\\' && !in_single_quote {
            index += ch.len_utf8();
            if index < s.len() {
                let next = s[index..].chars().next().expect("valid char boundary");
                out.push(next);
                index += next.len_utf8();
            }
            continue;
        }

        out.push(ch);
        index += ch.len_utf8();
    }

    (out, index)
}

#[cfg(test)]
mod tests {
    use super::{strip_sudo_prefix, SudoPayloadKind};

    #[test]
    fn preserves_shell_operators_after_stripping_leading_sudo() {
        let stripped = strip_sudo_prefix("sudo apt update && sudo apt install -y nginx");

        assert!(stripped.sudo_detected);
        assert!(stripped.ok);
        assert_eq!(stripped.command, "apt update && sudo apt install -y nginx");
        assert_eq!(stripped.user, None);
        assert_eq!(stripped.payload_kind(), Ok(SudoPayloadKind::Root));
    }

    #[test]
    fn strips_options_and_preserves_quotes() {
        let stripped = strip_sudo_prefix("sudo -E -u root bash -lc 'echo hi && echo ok'");

        assert!(stripped.sudo_detected);
        assert!(stripped.ok);
        assert_eq!(stripped.command, "bash -lc 'echo hi && echo ok'");
        assert_eq!(stripped.user.as_deref(), Some("root"));
        assert_eq!(stripped.payload_kind(), Ok(SudoPayloadKind::Root));
    }

    #[test]
    fn returns_failure_when_sudo_has_no_command() {
        let stripped = strip_sudo_prefix("sudo -E -u root");

        assert!(stripped.sudo_detected);
        assert!(!stripped.ok);
        assert!(stripped.command.is_empty());
        assert_eq!(stripped.user.as_deref(), Some("root"));
    }

    #[test]
    fn leaves_non_sudo_command_unchanged() {
        let stripped = strip_sudo_prefix("echo hello");

        assert!(!stripped.sudo_detected);
        assert!(stripped.ok);
        assert_eq!(stripped.command, "echo hello");
        assert_eq!(stripped.payload_kind(), Ok(SudoPayloadKind::NotSudo));
    }

    #[test]
    fn captures_user_and_group_flags() {
        let stripped = strip_sudo_prefix("sudo -u nobody -g nogroup id");

        assert_eq!(stripped.command, "id");
        assert_eq!(stripped.user.as_deref(), Some("nobody"));
        assert_eq!(stripped.group.as_deref(), Some("nogroup"));
    }

    #[test]
    fn captures_attached_and_equals_user_flags() {
        let attached = strip_sudo_prefix("sudo -unobody id");
        assert_eq!(attached.user.as_deref(), Some("nobody"));
        assert_eq!(attached.command, "id");

        let equals = strip_sudo_prefix("sudo --user=nobody id");
        assert_eq!(equals.user.as_deref(), Some("nobody"));
        assert_eq!(equals.command, "id");
    }

    #[test]
    fn numeric_user_without_passwd_still_resolves() {
        let stripped = strip_sudo_prefix("sudo -u 65534 id");
        assert_eq!(
            stripped.payload_kind(),
            Ok(SudoPayloadKind::User {
                uid: 65534,
                gid: lookup_gid_for_uid(65534),
            })
        );
    }

    #[test]
    fn unknown_user_is_rejected() {
        let stripped = strip_sudo_prefix("sudo -u aish-no-such-user-xyz id");
        assert_eq!(
            stripped.payload_kind(),
            Err("sudo_unknown_user".to_string())
        );
    }

    fn lookup_gid_for_uid(uid: u32) -> u32 {
        super::lookup_passwd_by_uid(uid).unwrap_or(uid)
    }
}
