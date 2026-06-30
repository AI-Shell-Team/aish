//! Styled prompts for the setup wizard (API base, API key, etc.).

use std::io;

use aish_core::AishError;
use aish_i18n::t;

fn acquire_guard() -> aish_tools::bash::InteractiveInputGuard {
    aish_tools::bash::acquire_interactive_input_guard()
}

/// Map cliclack `interact()` result: user cancel vs terminal I/O failure.
fn map_input_interact<T>(result: io::Result<T>) -> Result<T, AishError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(AishError::Cancelled),
        Err(error) => Err(AishError::Io(error)),
    }
}

/// Prompt for an HTTP(S) API base URL (cliclack header + input line).
pub fn prompt_api_base_url() -> Result<String, AishError> {
    let _guard = acquire_guard();

    let required_msg = t("cli.setup.provider_custom_api_base_required");
    let invalid_msg = t("cli.setup.provider_custom_api_base_invalid");
    let placeholder = t("cli.setup.custom_api_base_placeholder");
    let prompt = t("cli.setup.api_base_prompt");

    let required = required_msg.clone();
    let invalid = invalid_msg.clone();
    let result: String = map_input_interact(
        cliclack::input(&prompt)
            .placeholder(&placeholder)
            .validate(move |input: &String| {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    Err(required.clone())
                } else if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                    Err(invalid.clone())
                } else {
                    Ok(())
                }
            })
            .interact(),
    )?;

    Ok(result.trim().to_string())
}

/// Prompt for an API key with masked input (`•` per character, like Clack/OpenClaw).
pub fn prompt_api_key_value(env_value: Option<&str>) -> Result<String, AishError> {
    let _guard = acquire_guard();

    let required_msg = t("cli.setup.api_key_required");
    let prompt = t("cli.setup.api_key_prompt");
    let env_value = env_value.map(String::from);
    let has_env = env_value.is_some();

    let required = required_msg.clone();
    let mut password = cliclack::password(&prompt).mask('•');
    if has_env {
        password = password.allow_empty();
    }

    let result: String = map_input_interact(
        password
            .validate(move |input: &String| {
                if input.trim().is_empty() && !has_env {
                    Err(required.clone())
                } else {
                    Ok(())
                }
            })
            .interact(),
    )?;

    let trimmed = result.trim();
    if trimmed.is_empty() {
        return Ok(env_value.unwrap_or_default());
    }
    Ok(trimmed.to_string())
}

/// Prompt for a custom model name (cliclack header + input line).
pub fn prompt_custom_model_name() -> Result<String, AishError> {
    let _guard = acquire_guard();

    let prompt = t("cli.setup.model_custom_prompt");
    let required = t("cli.setup.model_custom_required");

    let result: String = map_input_interact(
        cliclack::input(&prompt)
            .validate(move |input: &String| {
                if input.trim().is_empty() {
                    Err(required.clone())
                } else {
                    Ok(())
                }
            })
            .interact(),
    )?;

    Ok(result.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_maps_to_cancelled() {
        let result = map_input_interact::<String>(Err(io::Error::from(io::ErrorKind::Interrupted)));
        assert!(matches!(result, Err(AishError::Cancelled)));
    }

    #[test]
    fn terminal_failure_propagates_as_io_error() {
        let result = map_input_interact::<String>(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        assert!(matches!(result, Err(AishError::Io(_))));
    }
}
