//! Inline terminal prompts using the `inquire` crate.
//!
//! Provides left-aligned inline selection and text input prompts.
//! Falls back to simple stdin prompts when a terminal is unavailable.

use std::io::{self, Write};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single selectable option presented in a dialog.
#[derive(Debug, Clone)]
pub struct DialogOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl DialogOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Format label for display, appending description if present.
    fn display_label(&self) -> String {
        match &self.description {
            Some(desc) => format!("{} - {}", self.label, desc),
            None => self.label.clone(),
        }
    }
}

/// Result returned by a dialog interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    /// User selected one of the predefined options (value field).
    Selected(String),
    /// User typed a custom answer.
    CustomInput(String),
    /// User cancelled / dismissed the dialog.
    Cancelled,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Show a selection dialog with predefined options using inline prompts.
///
/// Uses `inquire::Select` for option selection. If `allow_custom` is true,
/// a "(custom input)" option is appended; selecting it triggers a second
/// `inquire::Text` prompt for the custom value.
///
/// Falls back to a simple stdin numbered list when the terminal is unavailable.
pub fn show_selection_dialog(
    title: &str,
    question: &str,
    options: &[DialogOption],
    allow_custom: bool,
    allow_cancel: bool,
) -> DialogResult {
    match run_inquire_selection(title, question, options, allow_custom, allow_cancel) {
        Ok(result) => result,
        Err(_) => fallback_stdin_selection(title, question, options, allow_custom, allow_cancel),
    }
}

/// Show a simple Yes/No confirmation dialog.
pub fn show_confirmation_dialog(title: &str, message: &str) -> bool {
    let options = vec![
        DialogOption::new("yes", "Yes"),
        DialogOption::new("no", "No"),
    ];
    matches!(
        show_selection_dialog(title, message, &options, false, true),
        DialogResult::Selected(v) if v == "yes"
    )
}

// ---------------------------------------------------------------------------
// inquire implementation
// ---------------------------------------------------------------------------

/// Label used for the custom-input entry in the select list.
const CUSTOM_INPUT_LABEL: &str = "(type custom answer)";

fn run_inquire_selection(
    title: &str,
    question: &str,
    options: &[DialogOption],
    allow_custom: bool,
    allow_cancel: bool,
) -> Result<DialogResult, inquire::InquireError> {
    use inquire::Select;

    // Build display items. Each item is either a real option or the custom slot.
    #[derive(Clone)]
    enum Item {
        Real(usize), // index into `options`
        Custom,      // custom input slot
    }

    let mut items: Vec<(String, Item)> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| (opt.display_label(), Item::Real(i)))
        .collect();

    if allow_custom {
        items.push((CUSTOM_INPUT_LABEL.to_string(), Item::Custom));
    }

    let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();

    let ans = Select::new(&format!("{}: {}", title, question), labels)
        .with_help_message(if allow_cancel { "Esc to cancel" } else { "" })
        .prompt()?;

    // Find which item was selected.
    let idx = items.iter().position(|(l, _)| l == &ans).unwrap_or(0);
    let (_, ref item) = items[idx];

    match item {
        Item::Real(i) => Ok(DialogResult::Selected(options[*i].value.clone())),
        Item::Custom => {
            // Follow up with a text prompt for the custom value.
            let custom = inquire::Text::new(&format!("{}: enter custom value", title))
                .prompt()
                .unwrap_or_default();
            if custom.trim().is_empty() {
                Ok(DialogResult::Cancelled)
            } else {
                Ok(DialogResult::CustomInput(custom.trim().to_string()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback stdin implementation
// ---------------------------------------------------------------------------

fn fallback_stdin_selection(
    title: &str,
    question: &str,
    options: &[DialogOption],
    allow_custom: bool,
    allow_cancel: bool,
) -> DialogResult {
    // Print title
    println!("\x1b[1m{}\x1b[0m", title);
    // Print question
    println!("\x1b[36m{}\x1b[0m", question);

    // Print numbered options
    for (i, opt) in options.iter().enumerate() {
        if let Some(ref desc) = opt.description {
            println!("  \x1b[33m{}.\x1b[0m {} - {}", i + 1, opt.label, desc);
        } else {
            println!("  \x1b[33m{}.\x1b[0m {}", i + 1, opt.label);
        }
    }

    // Custom input option
    if allow_custom {
        println!("  \x1b[33m0.\x1b[0m \x1b[2m(type custom answer)\x1b[0m");
    }

    if allow_cancel {
        println!("  \x1b[2m(press Enter with empty input to cancel)\x1b[0m");
    }

    print!("Your answer: ");
    let _ = io::stdout().flush();

    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return DialogResult::Cancelled;
    }
    let answer = answer.trim().to_string();

    // Empty input -> cancel
    if answer.is_empty() {
        return DialogResult::Cancelled;
    }

    // Numeric selection
    if let Ok(num) = answer.parse::<usize>() {
        if num > 0 && num <= options.len() {
            return DialogResult::Selected(options[num - 1].value.clone());
        }
        // "0" means custom input but nothing typed -> cancel
        if num == 0 && allow_custom {
            return DialogResult::Cancelled;
        }
    }

    // Any other text -> custom input (if allowed) or treat as selected value
    if allow_custom {
        DialogResult::CustomInput(answer)
    } else {
        // Try to match against option labels as a convenience
        let matched = options
            .iter()
            .find(|o| o.label.eq_ignore_ascii_case(&answer));
        match matched {
            Some(o) => DialogResult::Selected(o.value.clone()),
            None => DialogResult::CustomInput(answer),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dialog_result_equality() {
        let r1 = DialogResult::Selected("yes".to_string());
        let r2 = DialogResult::Selected("yes".to_string());
        assert_eq!(r1, r2);

        let r3 = DialogResult::Selected("no".to_string());
        assert_ne!(r1, r3);

        let c1 = DialogResult::CustomInput("hello".to_string());
        let c2 = DialogResult::CustomInput("hello".to_string());
        assert_eq!(c1, c2);

        assert_eq!(DialogResult::Cancelled, DialogResult::Cancelled);

        assert_ne!(
            DialogResult::Selected("yes".to_string()),
            DialogResult::Cancelled
        );
        assert_ne!(
            DialogResult::CustomInput("yes".to_string()),
            DialogResult::Selected("yes".to_string())
        );
    }

    #[test]
    fn test_dialog_option_structure() {
        let opt = DialogOption::new("value1", "Label 1");
        assert_eq!(opt.value, "value1");
        assert_eq!(opt.label, "Label 1");
        assert!(opt.description.is_none());

        let opt_with_desc =
            DialogOption::new("value2", "Label 2").with_description("A description");
        assert_eq!(opt_with_desc.value, "value2");
        assert_eq!(opt_with_desc.label, "Label 2");
        assert_eq!(opt_with_desc.description.as_deref(), Some("A description"));

        let cloned = opt_with_desc.clone();
        assert_eq!(cloned.value, "value2");
        assert_eq!(cloned.description, Some("A description".to_string()));
    }

    #[test]
    fn test_display_label() {
        let opt = DialogOption::new("v", "Label");
        assert_eq!(opt.display_label(), "Label");

        let opt_desc = DialogOption::new("v", "Label").with_description("desc");
        assert_eq!(opt_desc.display_label(), "Label - desc");
    }
}
