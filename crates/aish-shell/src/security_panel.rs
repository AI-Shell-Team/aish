use aish_i18n::t_with_args;
use aish_llm::{PreflightSecurityContext, SecurityPanel};

pub(crate) fn build_security_panel(context: &PreflightSecurityContext) -> SecurityPanel {
    let Some(decision) = context.decision.as_ref() else {
        return SecurityPanel::fallback(
            context.tool_name.clone(),
            context.message.clone(),
            context.mode,
        );
    };

    let reasons = panel_reasons(decision);
    let alternatives = decision.analysis.suggested_alternatives.clone();
    let message = if reasons.is_empty() {
        context.message.clone()
    } else {
        reasons.join("; ")
    };

    SecurityPanel {
        mode: context.mode,
        tool_name: context.tool_name.clone(),
        target: context.target.clone(),
        message,
        risk_level: Some(decision.level.to_string()),
        reasons,
        alternatives,
    }
}

fn panel_reasons(decision: &aish_security::SecurityDecision) -> Vec<String> {
    let analysis = &decision.analysis;
    let mut reasons = Vec::new();

    if analysis.sandbox.enabled {
        reasons.extend(localized_sandbox_assessment_reasons(analysis));
    } else if let Some(reason) = analysis.sandbox.reason.as_deref() {
        reasons.push(localized_sandbox_degraded_reason(analysis, reason));
    }

    if analysis.fallback_rule_matched && !analysis.matched_paths.is_empty() {
        reasons.push(localized_preview_paths(&analysis.matched_paths));
    }

    reasons.extend(
        analysis
            .reasons
            .iter()
            .filter(|reason| !is_internal_security_reason(reason))
            .cloned(),
    );
    reasons.dedup();
    reasons
}

fn localized_sandbox_assessment_reasons(analysis: &aish_security::SecurityAnalysis) -> Vec<String> {
    let mut reasons = Vec::new();

    if analysis.changes.is_empty() {
        reasons.push(aish_i18n::t("security.ai_risk.no_fs_changes"));
    } else if !analysis.matched_paths.is_empty() {
        let mut args = std::collections::HashMap::new();
        args.insert(
            "count".to_string(),
            analysis.matched_paths.len().to_string(),
        );
        let key = match analysis.risk_level {
            aish_security::RiskLevel::High => "security.ai_risk.high_hits",
            aish_security::RiskLevel::Medium => "security.ai_risk.medium_hits",
            aish_security::RiskLevel::Low => "security.ai_risk.low_or_unmatched_hits",
        };
        reasons.push(aish_i18n::t_with_args(key, &args));
        reasons.push(localized_preview_paths(&analysis.matched_paths));
    } else {
        let mut args = std::collections::HashMap::new();
        args.insert("count".to_string(), analysis.changes.len().to_string());
        reasons.push(aish_i18n::t_with_args(
            "security.ai_risk.unmatched_hits",
            &args,
        ));
        let preview_paths: Vec<String> = analysis
            .changes
            .iter()
            .map(|change| change.path.clone())
            .take(3)
            .collect();
        if !preview_paths.is_empty() {
            reasons.push(localized_preview_paths(&preview_paths));
        }
    }

    if analysis
        .reasons
        .iter()
        .any(|reason| reason.starts_with("sandbox change list was truncated"))
    {
        reasons.push(aish_i18n::t("security.ai_risk.truncated_changes"));
    }

    reasons
}

fn localized_preview_paths(paths: &[String]) -> String {
    let preview_paths = paths
        .iter()
        .take(3)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let mut args = std::collections::HashMap::new();
    args.insert("paths".to_string(), preview_paths);
    t_with_args("security.ai_risk.preview_paths", &args)
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

    use super::build_security_panel;

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
}
