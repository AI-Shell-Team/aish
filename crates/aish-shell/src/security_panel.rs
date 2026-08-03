use aish_llm::{PreflightSecurityContext, SecurityPanel};

pub(crate) fn build_security_panel(context: &PreflightSecurityContext) -> SecurityPanel {
    let Some(decision) = context.decision.as_ref() else {
        return SecurityPanel::fallback(
            context.tool_name.clone(),
            context.message.clone(),
            context.mode,
        );
    };

    let reason = primary_panel_reason(decision, &context.message);
    let alternatives = decision.analysis.suggested_alternatives.clone();

    SecurityPanel {
        mode: context.mode,
        tool_name: context.tool_name.clone(),
        target: context.target.clone(),
        message: reason.clone(),
        risk_level: Some(decision.level.to_string()),
        reasons: if reason.trim().is_empty() {
            Vec::new()
        } else {
            vec![reason]
        },
        alternatives,
    }
}

pub(crate) fn security_panel_rows(context: &PreflightSecurityContext) -> Vec<(String, String)> {
    let mut rows = Vec::new();

    rows.push((
        aish_i18n::t("shell.confirm_dialog_tool")
            .trim_end_matches(['：', ':'])
            .to_string(),
        context.tool_name.clone(),
    ));

    if let Some(target) = context
        .target
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        rows.push((
            aish_i18n::t("shell.security.label.command"),
            target.to_string(),
        ));
    }

    let Some(decision) = context.decision.as_ref() else {
        if !context.message.trim().is_empty() {
            rows.push((
                aish_i18n::t("shell.security.label.reasons"),
                context.message.clone(),
            ));
        }
        return rows;
    };

    rows.push((
        aish_i18n::t("shell.security.label.risk_level"),
        decision.level.to_string(),
    ));

    let reason = primary_panel_reason(decision, &context.message);
    if !reason.trim().is_empty() {
        rows.push((aish_i18n::t("shell.security.label.reasons"), reason.clone()));
    }

    if let Some(rule) = matched_rule_label(decision) {
        rows.push((aish_i18n::t("shell.security.label.rule"), rule));
    }

    if !decision.analysis.matched_paths.is_empty() {
        let paths = decision
            .analysis
            .matched_paths
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if !paths.is_empty() {
            rows.push((aish_i18n::t("shell.security.label.matched_paths"), paths));
        }
    }

    // Paths and degraded-sandbox notes are independent: a fallback decision can
    // carry matched paths while still needing to disclose that sandbox was down.
    if let Some(sandbox_reason) = decision.analysis.sandbox.reason.as_deref() {
        let note = localized_sandbox_degraded_reason(&decision.analysis, sandbox_reason);
        if !note.trim().is_empty() && reason != note {
            rows.push((aish_i18n::t("shell.security.label.fallback_hint"), note));
        }
    }

    if !decision.analysis.suggested_alternatives.is_empty() {
        rows.push((
            aish_i18n::t("shell.security.label.alternatives"),
            decision.analysis.suggested_alternatives.join("\n"),
        ));
    }

    rows
}

fn panel_title_key(mode: aish_llm::SecurityPanelMode) -> &'static str {
    match mode {
        aish_llm::SecurityPanelMode::Confirm => "shell.security.panel_title.confirm",
        aish_llm::SecurityPanelMode::Blocked => "shell.security.panel_title.blocked",
        aish_llm::SecurityPanelMode::Info => "shell.security.panel_title.notice",
    }
}

pub(crate) fn security_panel_title(mode: aish_llm::SecurityPanelMode) -> String {
    aish_i18n::t(panel_title_key(mode))
}

fn primary_panel_reason(decision: &aish_security::SecurityDecision, fallback: &str) -> String {
    if let Some(reason) = decision
        .analysis
        .reasons
        .iter()
        .find(|reason| !is_internal_security_reason(reason))
    {
        return reason.clone();
    }

    let impact = decision.analysis.impact_description.trim();
    if !impact.is_empty() {
        return impact.to_string();
    }

    if let Some(sandbox_reason) = decision.analysis.sandbox.reason.as_deref() {
        if !decision.analysis.sandbox.enabled {
            return localized_sandbox_degraded_reason(&decision.analysis, sandbox_reason);
        }
    }

    strip_message_annotation(fallback)
}

fn matched_rule_label(decision: &aish_security::SecurityDecision) -> Option<String> {
    let rule = decision.analysis.matched_rule.as_ref()?;
    let mut identity = String::new();
    if let Some(id) = rule
        .id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        identity.push_str(id);
    }
    if let Some(name) = rule
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !identity.is_empty() {
            identity.push_str(" — ");
        }
        identity.push_str(name);
    }
    if identity.is_empty() {
        None
    } else {
        Some(identity)
    }
}

fn strip_message_annotation(message: &str) -> String {
    // format_security_message may append " (H-001; paths: ...)" for LLM/block text.
    let trimmed = message.trim();
    if let Some(idx) = trimmed.rfind(" (") {
        let suffix = &trimmed[idx + 2..];
        let looks_like_rule_id = |part: &str| {
            let part = part.trim();
            matches!(part.as_bytes().first(), Some(b'H' | b'M' | b'L'))
                && part.len() > 2
                && part.as_bytes()[1] == b'-'
                && part[2..].bytes().all(|b| b.is_ascii_digit())
        };
        if suffix.ends_with(')')
            && (suffix.contains("paths:")
                || suffix
                    .trim_end_matches(')')
                    .split(';')
                    .any(looks_like_rule_id))
        {
            return trimmed[..idx].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn localized_sandbox_degraded_reason(
    analysis: &aish_security::SecurityAnalysis,
    reason: &str,
) -> String {
    let mut args = std::collections::HashMap::new();
    let action = analysis
        .sandbox_off_action
        .map(localized_sandbox_off_action)
        .unwrap_or_else(|| aish_i18n::t("security.sandbox_off_action.confirm"));
    args.insert("action".to_string(), action);
    if let Some(cwd) = analysis.sandbox.cwd.as_deref() {
        args.insert("cwd".to_string(), cwd.to_string());
    }
    if let Some(root) = analysis.sandbox.repo_root.as_deref() {
        args.insert("root".to_string(), root.to_string());
    }

    match reason {
        "sandbox_ipc_unavailable" => aish_i18n::t("security.sandbox_unavailable.ipc_unavailable"),
        "sandbox_ipc_failed" => aish_i18n::t("security.sandbox_unavailable.ipc_failed"),
        "sandbox_ipc_timeout" => aish_i18n::t("shell.security.fallback.sandbox_ipc_timeout"),
        "sandbox_execute_failed" => {
            aish_i18n::t("security.sandbox_unavailable.sandbox_execute_failed")
        }
        "sandbox_timeout" => aish_i18n::t("shell.security.fallback.sandbox_timeout"),
        "sandbox_disabled_by_policy" => {
            aish_i18n::t_with_args("security.risk_reason.sandbox_disabled_by_policy", &args)
        }
        "sandbox_disabled" => {
            aish_i18n::t_with_args("security.risk_reason.sandbox_disabled", &args)
        }
        "cwd_outside_repo_root" => {
            aish_i18n::t_with_args("security.risk_reason.cwd_outside_repo_root", &args)
        }
        "sandbox_unavailable" => {
            aish_i18n::t_with_args("security.risk_reason.sandbox_unavailable", &args)
        }
        "sandbox_exception" => {
            aish_i18n::t_with_args("security.risk_reason.sandbox_exception", &args)
        }
        "sandbox_failed" => aish_i18n::t_with_args("security.risk_reason.sandbox_failed", &args),
        _ => reason.to_string(),
    }
}

fn localized_sandbox_off_action(action: aish_security::SandboxOffAction) -> String {
    match action {
        aish_security::SandboxOffAction::Allow => aish_i18n::t("security.sandbox_off_action.allow"),
        aish_security::SandboxOffAction::Confirm => {
            aish_i18n::t("security.sandbox_off_action.confirm")
        }
        aish_security::SandboxOffAction::Block => aish_i18n::t("security.sandbox_off_action.block"),
    }
}

fn is_internal_security_reason(reason: &str) -> bool {
    [
        "sandbox degraded because ",
        "sandbox exit_code:",
        "policy fallback rule matched for command ",
        "matched paths: ",
        "sandbox matched ",
        "sandbox observed no filesystem changes;",
        "sandbox changes matched no policy rule;",
        "unmatched paths: ",
        "sandbox change list was truncated;",
        "sandbox execution returned non-zero exit code ",
    ]
    .iter()
    .any(|prefix| reason.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use aish_llm::{PreflightSecurityContext, SecurityPanelMode};
    use aish_security::{MatchedRuleSummary, RiskLevel, SecurityAnalysis, SecurityDecision};

    use super::{build_security_panel, security_panel_rows, strip_message_annotation};

    fn blocked_etc_context() -> PreflightSecurityContext {
        let mut analysis = SecurityAnalysis {
            risk_level: RiskLevel::High,
            reasons: vec![
                "System config changes can break the host".to_string(),
                "sandbox matched 1 high-risk filesystem change(s)".to_string(),
            ],
            impact_description: "System config changes can break the host".to_string(),
            suggested_alternatives: vec![
                "Edit /etc manually with a backup/change process.".to_string()
            ],
            matched_rule: Some(MatchedRuleSummary {
                id: Some("H-001".to_string()),
                name: Some("Protect /etc".to_string()),
                pattern: "/etc/**".to_string(),
            }),
            matched_paths: vec!["/etc/aish/123".to_string()],
            ..Default::default()
        };
        analysis.sandbox.enabled = true;
        let decision = SecurityDecision::block(RiskLevel::High, analysis);
        PreflightSecurityContext {
            tool_name: "bash".to_string(),
            target: Some("rm /etc/aish/123".to_string()),
            message: "System config changes can break the host (H-001; paths: /etc/aish/123)"
                .to_string(),
            mode: SecurityPanelMode::Blocked,
            decision: Some(decision),
        }
    }

    #[test]
    fn fallback_context_builds_fallback_panel() {
        let context = PreflightSecurityContext::fallback(
            "bash",
            Some("rm /tmp/x".to_string()),
            "generic security message",
            SecurityPanelMode::Confirm,
        );

        let panel = build_security_panel(&context);

        assert_eq!(panel.tool_name, "bash");
        assert_eq!(panel.message, "generic security message");
        assert!(panel.reasons.is_empty());
        assert!(panel.alternatives.is_empty());
    }

    #[test]
    fn decision_context_builds_primary_reason_message() {
        let context = blocked_etc_context();
        let panel = build_security_panel(&context);

        assert_eq!(panel.risk_level.as_deref(), Some("HIGH"));
        assert_eq!(panel.message, "System config changes can break the host");
        assert_eq!(
            panel.reasons,
            vec!["System config changes can break the host".to_string()]
        );
        assert_eq!(
            panel.alternatives,
            vec!["Edit /etc manually with a backup/change process.".to_string()]
        );
    }

    #[test]
    fn security_panel_rows_separate_reason_rule_and_paths() {
        let context = blocked_etc_context();
        let rows = security_panel_rows(&context);
        let map: std::collections::HashMap<&str, &str> = rows
            .iter()
            .map(|(label, value)| (label.as_str(), value.as_str()))
            .collect();

        let reason = rows
            .iter()
            .find(|(label, _)| {
                label.contains("Reason")
                    || label.contains("原因")
                    || label.contains("理由")
                    || label.contains("Motivos")
                    || label.contains("Raisons")
                    || label.contains("Gründe")
            })
            .map(|(_, value)| value.as_str())
            .expect("reason row");
        assert_eq!(reason, "System config changes can break the host");
        assert!(
            !reason.contains("H-001"),
            "primary reason must not mash rule id into the same value: {reason}"
        );

        let rule = rows
            .iter()
            .find(|(label, _)| {
                label.contains("Rule")
                    || label.contains("规则")
                    || label.contains("ルール")
                    || label.contains("Regel")
                    || label.contains("Règle")
                    || label.contains("Regla")
            })
            .map(|(_, value)| value.as_str())
            .expect("rule row");
        assert!(
            rule.contains("H-001") && rule.contains("Protect /etc"),
            "{rule}"
        );

        let paths = rows
            .iter()
            .find(|(label, _)| {
                label.contains("Path")
                    || label.contains("路径")
                    || label.contains("パス")
                    || label.contains("Pfad")
                    || label.contains("Chemin")
                    || label.contains("Ruta")
            })
            .map(|(_, value)| value.as_str())
            .expect("paths row");
        assert_eq!(paths, "/etc/aish/123");

        assert!(map.values().any(|value| value.contains("HIGH")));
        assert!(rows.iter().any(|(_, value)| {
            value.contains("Edit /etc manually with a backup/change process.")
        }));
    }

    fn degraded_with_paths_context() -> PreflightSecurityContext {
        let mut analysis = SecurityAnalysis {
            risk_level: RiskLevel::Medium,
            reasons: vec!["home path is protected".to_string()],
            impact_description: "home path is protected".to_string(),
            matched_rule: Some(MatchedRuleSummary {
                id: Some("M-001".to_string()),
                name: Some("Protect /home".to_string()),
                pattern: "/home/**".to_string(),
            }),
            matched_paths: vec!["/home/lixin/123".to_string()],
            ..Default::default()
        };
        analysis.sandbox.enabled = false;
        analysis.sandbox.reason = Some("sandbox_ipc_unavailable".to_string());
        let decision = SecurityDecision::confirm(RiskLevel::Medium, analysis);
        PreflightSecurityContext {
            tool_name: "bash".to_string(),
            target: Some("rm /home/lixin/123".to_string()),
            message: "home path is protected (M-001; paths: /home/lixin/123)".to_string(),
            mode: SecurityPanelMode::Confirm,
            decision: Some(decision),
        }
    }

    fn row_value<'a>(rows: &'a [(String, String)], needles: &[&str]) -> Option<&'a str> {
        rows.iter()
            .find(|(label, _)| needles.iter().any(|n| label.contains(n)))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn security_panel_rows_show_paths_and_sandbox_fallback_hint() {
        let context = degraded_with_paths_context();
        let rows = security_panel_rows(&context);

        let paths = row_value(&rows, &["Path", "路径", "パス", "Pfad", "Chemin", "Ruta"])
            .expect("paths row");
        assert_eq!(paths, "/home/lixin/123");

        let note = row_value(
            &rows,
            &["Note", "提示", "Hinweis", "Nota", "Remarque", "注記"],
        )
        .expect("fallback hint row");
        assert!(
            !note.trim().is_empty(),
            "expected degraded-sandbox note, got empty"
        );
        assert_ne!(note, "home path is protected");
    }

    #[test]
    fn strip_message_annotation_keeps_host_unreachable_parenthetical() {
        let message = "cannot verify command (Host unreachable)";
        assert_eq!(strip_message_annotation(message), message);
    }

    #[test]
    fn strip_message_annotation_strips_rule_id_and_paths() {
        assert_eq!(
            strip_message_annotation(
                "System config changes can break the host (H-001; paths: /etc/aish/123)"
            ),
            "System config changes can break the host"
        );
        assert_eq!(
            strip_message_annotation("home path is protected (M-001)"),
            "home path is protected"
        );
    }
}
