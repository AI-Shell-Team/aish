pub fn strip_sudo_prefix(command: &str) -> (String, bool, bool) {
    let raw = command;
    let raw_l = raw.trim_start();
    if !raw_l.starts_with("sudo ") && raw_l != "sudo" {
        return (command.to_string(), false, true);
    }

    let (token, next_index) = read_token(raw_l, 0);
    if token != "sudo" {
        return (command.to_string(), false, true);
    }

    let mut index = next_index;
    let options_with_value = ["-u", "--user", "-g", "--group", "-h", "-p", "--prompt"];

    loop {
        index = skip_ws(raw_l, index);
        if index >= raw_l.len() {
            return (String::new(), true, false);
        }

        let (opt, opt_end) = read_token(raw_l, index);
        if opt.is_empty() {
            return (String::new(), true, false);
        }

        if opt == "--" {
            index = opt_end;
            break;
        }

        if opt.starts_with('-') {
            if options_with_value.contains(&opt.as_str()) {
                index = opt_end;
                let (_, value_end) = read_token(raw_l, index);
                index = value_end;
                continue;
            }

            if (opt.starts_with("-u") && opt != "-u") || (opt.starts_with("-g") && opt != "-g") {
                index = opt_end;
                continue;
            }

            if opt.starts_with("--user=")
                || opt.starts_with("--group=")
                || opt.starts_with("--prompt=")
            {
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
        return (String::new(), true, false);
    }

    (stripped, true, true)
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
    use super::strip_sudo_prefix;

    #[test]
    fn preserves_shell_operators_after_stripping_leading_sudo() {
        let (stripped, sudo_detected, ok) =
            strip_sudo_prefix("sudo apt update && sudo apt install -y nginx");

        assert!(sudo_detected);
        assert!(ok);
        assert_eq!(stripped, "apt update && sudo apt install -y nginx");
    }

    #[test]
    fn strips_options_and_preserves_quotes() {
        let (stripped, sudo_detected, ok) =
            strip_sudo_prefix("sudo -E -u root bash -lc 'echo hi && echo ok'");

        assert!(sudo_detected);
        assert!(ok);
        assert_eq!(stripped, "bash -lc 'echo hi && echo ok'");
    }

    #[test]
    fn returns_failure_when_sudo_has_no_command() {
        let (stripped, sudo_detected, ok) = strip_sudo_prefix("sudo -E -u root");

        assert!(sudo_detected);
        assert!(!ok);
        assert!(stripped.is_empty());
    }

    #[test]
    fn leaves_non_sudo_command_unchanged() {
        let (stripped, sudo_detected, ok) = strip_sudo_prefix("echo hello");

        assert!(!sudo_detected);
        assert!(ok);
        assert_eq!(stripped, "echo hello");
    }
}
