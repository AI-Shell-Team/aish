// Terminal formatting for plan markdown documents.
//
// Applies theme-based formatting to plan content for display in the
// plan approval flow: bold+accent headings, dim code blocks, bullet chars.

use crate::theme;

/// Format a plan markdown document for terminal display.
///
/// Applies the following transformations:
/// - `## Headings` → bold + accent color
/// - `` ```code blocks``` `` → indented with dim color
/// - `- bullet points` → bullet character
/// - numbered items preserved as-is
pub fn format_plan_for_display(content: &str) -> String {
    let mut output = String::new();
    let mut in_code_block = false;
    let mut code_buffer = String::new();

    for line in content.lines() {
        // Handle code block delimiters
        if line.trim_start().starts_with("```") {
            if in_code_block {
                // End code block
                in_code_block = false;
                // Format buffered code lines
                for code_line in code_buffer.lines() {
                    output.push_str(&format!("{}\n", theme::dim(&format!("  │ {}", code_line))));
                }
                output.push_str(&format!("{}\n", theme::dim("  └───")));
                code_buffer.clear();
            } else {
                // Start code block
                in_code_block = true;
                code_buffer.clear();
                // Extract language hint if present
                let lang = line.trim_start().trim_start_matches('`').trim();
                if !lang.is_empty() {
                    output.push_str(&format!(
                        "{}\n",
                        theme::dim(&format!("  ┌─── {} ───", lang))
                    ));
                } else {
                    output.push_str(&format!("{}\n", theme::dim("  ┌───")));
                }
            }
            continue;
        }

        if in_code_block {
            code_buffer.push_str(line);
            code_buffer.push('\n');
            continue;
        }

        // Format headings (## and ###)
        let trimmed = line.trim();
        if trimmed.starts_with("### ") {
            let heading = trimmed.trim_start_matches('#').trim();
            output.push_str(&format!(
                "\n{}\n\n",
                theme::bold(&theme::accent(&format!("  {}", heading)))
            ));
        } else if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
            let heading = trimmed.trim_start_matches('#').trim();
            output.push_str(&format!(
                "\n{}\n",
                theme::bold(&theme::accent(&format!("  {}", heading)))
            ));
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            // Convert markdown bullet markers to bullet character
            let item_text = &trimmed[2..];
            output.push_str(&format!("    {}{}{}", "\u{2022} ", item_text, "\n"));
        } else if trimmed.is_empty() {
            output.push('\n');
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_headings() {
        let input = "# Title\n## Section\n### Subsection\n";
        let output = format_plan_for_display(input);

        assert!(output.contains("Title"));
        assert!(output.contains("Section"));
        assert!(output.contains("Subsection"));
        // Headings are indented with two spaces
        assert!(output.contains("  Title"));
        assert!(output.contains("  Section"));
        assert!(output.contains("  Subsection"));
    }

    #[test]
    fn test_format_bullet_points() {
        let input = "- First item\n- Second item\n* Third item\n";
        let output = format_plan_for_display(input);

        assert!(output.contains('\u{2022}'));
        assert!(output.contains("First item"));
        assert!(output.contains("Second item"));
        assert!(output.contains("Third item"));
    }

    #[test]
    fn test_format_code_block() {
        let input = "```rust\nfn main() {}\n```\n";
        let output = format_plan_for_display(input);

        assert!(output.contains("rust"));
        assert!(output.contains("fn main() {}"));
        assert!(output.contains("┌───"));
        assert!(output.contains('\u{2502}')); // │ box drawing
    }

    #[test]
    fn test_format_plain_text() {
        let input = "Just some plain text\nMore text\n";
        let output = format_plan_for_display(input);

        assert!(output.contains("Just some plain text"));
        assert!(output.contains("More text"));
    }

    #[test]
    fn test_format_empty_input() {
        let output = format_plan_for_display("");
        // Empty input yields empty output (no styling needed)
        assert!(output.is_empty());
    }

    #[test]
    fn test_format_numbered_items_preserved() {
        let input = "1. First step\n2. Second step\n3. Third step\n";
        let output = format_plan_for_display(input);

        assert!(output.contains("1. First step"));
        assert!(output.contains("2. Second step"));
        assert!(output.contains("3. Third step"));
    }

    #[test]
    fn test_format_full_plan() {
        let input = r#"# Plan

## Overview
This is the plan overview.

## Implementation Steps
1. First step
2. Second step

## Code Example
```bash
echo "hello"
```

## Notes
- Important note
- Another note
"#;
        let output = format_plan_for_display(input);

        assert!(output.contains("Plan"));
        assert!(output.contains("Overview"));
        assert!(output.contains("Implementation Steps"));
        assert!(output.contains("1. First step"));
        assert!(output.contains("echo"));
        assert!(output.contains('\u{2022}'));
        // Headings indented; code fence marker present
        assert!(output.contains("  Plan"));
        assert!(output.contains("┌───"));
    }
}
