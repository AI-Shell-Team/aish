//! Setup wizard presentation via [cliclack](https://github.com/AI-Shell-Team/cliclack).

use std::io;

use aish_core::AishError;
use aish_i18n::t;

use crate::tui::{DialogOption, DialogResult, CUSTOM_DIALOG_VALUE};
use crate::wizard::clack_theme;

const LIST_PAGE_SIZE: usize = 12;

fn clack_message(title: &str, subtitle: &str) -> String {
    if subtitle.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title}\n{subtitle}")
    }
}

fn clack_select(
    message: &str,
    options: &[DialogOption],
    searchable: bool,
    search_prefix: Option<&str>,
    footer: Option<&str>,
) -> Result<DialogResult, io::Error> {
    let _guard = aish_tools::bash::acquire_interactive_input_guard();
    let search = searchable.then(|| {
        let haystacks: Vec<String> = options
            .iter()
            .map(|opt| {
                cliclack::compose_filter_haystack(
                    &opt.label,
                    opt.description.as_deref().unwrap_or(""),
                    &opt.value,
                )
            })
            .collect();
        clack_theme::SearchContext {
            prefix: search_prefix.unwrap_or("").to_string(),
            haystacks,
            no_matches: aish_i18n::t("cli.setup.filter_no_results"),
        }
    });
    let _prompt_ctx = clack_theme::PromptContextGuard::new(footer, search);

    if options.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "setup select called with no options",
        ));
    }

    let mut prompt = cliclack::select(message);
    for opt in options {
        let hint = opt.description.as_deref().unwrap_or("");
        prompt = prompt.item(opt.value.clone(), &opt.label, hint);
    }
    if searchable {
        prompt = prompt.filter_mode();
    }
    prompt = prompt.max_rows(LIST_PAGE_SIZE);

    match prompt.interact() {
        Ok(value) => Ok(DialogResult::Selected(value)),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(DialogResult::Cancelled),
        Err(error) => Err(error),
    }
}

fn map_prompt_io(result: Result<DialogResult, io::Error>) -> Result<DialogResult, AishError> {
    result.map_err(AishError::Io)
}

fn merge_custom_dialog_options(
    options: &[DialogOption],
    custom_label: Option<&str>,
    custom_first: bool,
) -> Vec<DialogOption> {
    let mut merged: Vec<DialogOption> = options.to_vec();
    if let Some(label) = custom_label {
        let custom = DialogOption::new(CUSTOM_DIALOG_VALUE, label);
        if custom_first {
            merged.insert(0, custom);
        } else {
            merged.push(custom);
        }
    }
    merged
}

/// Simple selection (entry mode, verify retry menus).
pub fn show_selection(
    title: &str,
    question: &str,
    options: &[DialogOption],
) -> Result<DialogResult, AishError> {
    map_prompt_io(clack_select(
        &clack_message(title, question),
        options,
        false,
        None,
        Some(&t("cli.setup.select_hint")),
    ))
}

/// Searchable selection (provider, endpoint, model).
pub fn show_searchable_selection(
    title: &str,
    search_placeholder: &str,
    footer: &str,
    options: &[DialogOption],
    custom_label: Option<&str>,
    custom_first: bool,
) -> Result<DialogResult, AishError> {
    let merged = merge_custom_dialog_options(options, custom_label, custom_first);
    map_prompt_io(clack_select(
        title,
        &merged,
        true,
        Some(search_placeholder),
        Some(footer),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_cancel_is_not_io_error() {
        let interact_result: io::Result<String> = Err(io::Error::from(io::ErrorKind::Interrupted));
        let dialog = match interact_result {
            Ok(value) => Ok(DialogResult::Selected(value)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(DialogResult::Cancelled),
            Err(error) => Err(error),
        };
        assert_eq!(dialog.unwrap(), DialogResult::Cancelled);
    }

    #[test]
    fn terminal_failure_propagates_as_io_error() {
        let err = io::Error::from(io::ErrorKind::BrokenPipe);
        let result = map_prompt_io(Err(err));
        assert!(matches!(result, Err(AishError::Io(_))));
    }
}
