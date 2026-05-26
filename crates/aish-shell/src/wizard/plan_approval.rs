// Plan approval flow for interactive plan review.
//
// Displays a formatted plan to the user and collects their decision:
// approve, request changes with feedback, or cancel.

use std::io::{self, Write};

use super::plan_display::format_plan_for_display;

/// User decision after reviewing a plan.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanApprovalDecision {
    /// User approved the plan.
    Approved,
    /// User wants changes; contains their feedback text.
    ChangesRequested { feedback: String },
    /// User cancelled the plan review entirely.
    Cancelled,
}

/// Interactive plan review and approval flow.
///
/// Displays a formatted plan and prompts the user to approve,
/// request changes, or cancel.
pub struct PlanApprovalFlow;

impl PlanApprovalFlow {
    /// Display plan content formatted for terminal and collect user decision.
    ///
    /// # Arguments
    /// * `plan_content` - The full markdown text of the plan artifact
    /// * `summary` - Optional one-line summary shown in the header
    /// * `revision` - Optional revision number to display
    ///
    /// # Returns
    /// The user's decision: Approved, ChangesRequested with feedback, or Cancelled.
    pub fn review_plan(
        plan_content: &str,
        summary: Option<&str>,
        revision: Option<i32>,
    ) -> PlanApprovalDecision {
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(80);

        // Header
        println!();
        let header_text = " Plan Review ";
        let header_pad = width.saturating_sub(header_text.len());
        let left_pad = header_pad / 2;
        let right_pad = header_pad - left_pad;
        println!(
            "\x1b[1;33m\u{2550}{}\u{2550}\u{2550}{}\u{2550}\x1b[0m",
            "\u{2550}".repeat(left_pad),
            "\u{2550}".repeat(right_pad)
        );
        println!(
            "\x1b[1;33m\u{2550}{}\u{2550}\u{2550}{}\u{2550}\x1b[0m",
            " ".repeat(left_pad),
            " ".repeat(right_pad)
        );
        // Center the header text
        println!(
            "\x1b[1;33m\u{2550}\x1b[1m{}\x1b[1;33m\u{2550}\x1b[0m",
            center_text(header_text, width)
        );
        println!(
            "\x1b[1;33m\u{2550}{}\u{2550}\u{2550}{}\u{2550}\x1b[0m",
            " ".repeat(left_pad),
            " ".repeat(right_pad)
        );
        println!(
            "\x1b[1;33m\u{2550}{}\u{2550}\u{2550}{}\u{2550}\x1b[0m",
            "\u{2550}".repeat(left_pad),
            "\u{2550}".repeat(right_pad)
        );

        // Summary line
        if let Some(s) = summary {
            if !s.is_empty() {
                println!("\x1b[1m  Summary:\x1b[0m {}", s);
            }
        }

        // Revision number
        if let Some(rev) = revision {
            println!("\x1b[2m  Revision: #{}\x1b[0m", rev);
        }

        println!();

        // Display formatted plan content
        let formatted = format_plan_for_display(plan_content);
        // Print with a slight indent
        for line in formatted.lines() {
            println!("  {}", line);
        }

        println!();

        // Separator
        println!(
            "\x1b[33m\u{2500}{}\x1b[0m",
            "\u{2500}".repeat(width.saturating_sub(1))
        );

        // Options prompt
        println!("\x1b[1m  Choose an action:\x1b[0m");
        println!("    \x1b[32m[A]\x1b[0m Approve plan and proceed");
        println!("    \x1b[33m[R]\x1b[0m Request changes to the plan");
        println!("    \x1b[31m[C]\x1b[0m Cancel plan mode");
        print!("\n  Your choice: ");
        let _ = io::stdout().flush();

        // Read single character choice
        let mut choice = String::new();
        if io::stdin().read_line(&mut choice).is_err() {
            return PlanApprovalDecision::Cancelled;
        }
        let choice = choice.trim().to_uppercase();

        match choice.as_str() {
            "A" | "APPROVE" | "Y" | "YES" => PlanApprovalDecision::Approved,
            "R" | "REQUEST" => {
                // Prompt for feedback
                println!();
                println!("\x1b[1;33m  Please describe the changes you'd like:\x1b[0m");
                print!("  > ");
                let _ = io::stdout().flush();

                let mut feedback = String::new();
                if io::stdin().read_line(&mut feedback).is_err() {
                    return PlanApprovalDecision::ChangesRequested {
                        feedback: String::new(),
                    };
                }
                let feedback = feedback.trim().to_string();

                if feedback.is_empty() {
                    println!("\x1b[2m  (No feedback provided)\x1b[0m");
                }

                PlanApprovalDecision::ChangesRequested { feedback }
            }
            "C" | "CANCEL" | "N" | "NO" | "" => PlanApprovalDecision::Cancelled,
            _ => {
                // Unknown choice, treat as cancel
                println!("\x1b[33m  Unknown option. Cancelling plan review.\x1b[0m");
                PlanApprovalDecision::Cancelled
            }
        }
    }
}

/// Center text within a given width, padding with spaces on both sides.
fn center_text(text: &str, width: usize) -> String {
    let text_len = text.chars().count();
    if text_len >= width {
        return text.to_string();
    }
    let pad = width.saturating_sub(text_len);
    let left = pad / 2;
    let right = pad - left;
    format!(
        "{}{}\x1b[1;33m{}\x1b[0m",
        " ".repeat(left),
        text,
        " ".repeat(right)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_equality() {
        let d1 = PlanApprovalDecision::Approved;
        let d2 = PlanApprovalDecision::Approved;
        assert_eq!(d1, d2);

        let d3 = PlanApprovalDecision::ChangesRequested {
            feedback: "fix this".to_string(),
        };
        assert_ne!(d1, d3);

        let d4 = PlanApprovalDecision::Cancelled;
        assert_ne!(d1, d4);
    }

    #[test]
    fn test_center_text_short() {
        let result = center_text("hi", 10);
        assert!(result.contains("hi"));
        // Should have padding
        assert!(result.len() > 2);
    }

    #[test]
    fn test_center_text_exact() {
        let result = center_text("hello", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_center_text_long() {
        let result = center_text("hello world", 5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_format_plan_integration() {
        let plan = "## Overview\nThis is a test plan.\n\n## Steps\n1. Do something\n";
        let formatted = format_plan_for_display(plan);
        assert!(formatted.contains("Overview"));
        assert!(formatted.contains("Steps"));
        assert!(formatted.contains("Do something"));
    }
}
