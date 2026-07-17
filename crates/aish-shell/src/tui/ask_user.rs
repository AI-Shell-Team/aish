use std::io::{self, IsTerminal};

use aish_i18n::{t, t_with_args};
use aish_tools::ask_user::{AskUserRequest, AskUserResponse};
use aish_ui::{
    ChoiceOutcome, ChoicePanel, PanelComponent, PanelEvent, PanelOutcome, PanelRuntime,
    SearchSelectItem,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

pub fn run_ask_user_request(request: &AskUserRequest) -> io::Result<AskUserResponse> {
    let _input_guard = aish_tools::bash::acquire_interactive_input_guard();

    AskUserSession::new(request).run()
}

struct AskUserSession<'a> {
    request: &'a AskUserRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TextResolution {
    Answer(String),
    Invalid(TextValidationError),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TextValidationError {
    Required,
    MinLength(usize),
}

impl TextValidationError {
    fn message(&self) -> String {
        match self {
            Self::Required => t("shell.ask_user.required_error"),
            Self::MinLength(min) => t_with_args(
                "shell.ask_user.min_length_error",
                &std::collections::HashMap::from([("min".to_string(), min.to_string())]),
            ),
        }
    }
}

impl<'a> AskUserSession<'a> {
    fn new(request: &'a AskUserRequest) -> Self {
        Self { request }
    }

    fn run(&self) -> io::Result<AskUserResponse> {
        self.ensure_terminal()?;

        if self.request.options.is_empty() {
            self.run_text_input()
        } else {
            self.run_choice_input()
        }
    }

    fn ensure_terminal(&self) -> io::Result<()> {
        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            Ok(())
        } else {
            Err(io::Error::other(t(
                "shell.ask_user.interactive_terminal_required",
            )))
        }
    }

    fn run_choice_input(&self) -> io::Result<AskUserResponse> {
        loop {
            let items: Vec<SearchSelectItem> = self
                .request
                .options
                .iter()
                .map(|option| {
                    let mut item =
                        SearchSelectItem::new(option.value.clone(), option.label.clone());
                    if let Some(description) = &option.description {
                        item = item.with_detail(description.clone());
                    }
                    if option.recommended {
                        item = item.with_badge(t("shell.ask_user.recommended_badge"));
                    }
                    item.with_search_text(format!(
                        "{} {} {}",
                        option.value,
                        option.label,
                        option.description.as_deref().unwrap_or("")
                    ))
                })
                .collect();

            let default_title = t("shell.ask_user.default_title");
            let title = self
                .request
                .title
                .as_deref()
                .unwrap_or(default_title.as_str());
            let footer = t("shell.ask_user.footer");
            let mut panel = ChoicePanel::new(title, self.request.prompt.as_str(), items)
                .with_allow_cancel(self.request.allow_cancel)
                .with_footer(footer.clone())
                .with_custom_input_footer(footer)
                .with_selected_value(self.request.default.as_deref());

            if self.request.allow_freeform_input {
                panel = panel.with_custom_label(t("shell.ask_user.custom_input_label"));
            }

            if let Some(placeholder) = &self.request.placeholder {
                panel = panel.with_custom_input_footer(placeholder.clone());
            }

            match PanelRuntime::new().run(panel).map_err(io::Error::other)? {
                PanelOutcome::Submitted(ChoiceOutcome::Selected(value)) => {
                    if let Some(option) = self
                        .request
                        .options
                        .iter()
                        .find(|option| option.value == value)
                    {
                        return Ok(AskUserResponse::Selected {
                            value: option.value.clone(),
                            label: option.label.clone(),
                            description: option.description.clone(),
                        });
                    }

                    match self.resolve_text_answer(value) {
                        TextResolution::Answer(answer) => return Ok(AskUserResponse::Text(answer)),
                        TextResolution::Invalid(error) => {
                            print_validation_error(&error);
                            continue;
                        }
                        TextResolution::Cancelled => return Ok(AskUserResponse::Cancelled),
                    }
                }
                PanelOutcome::Submitted(ChoiceOutcome::CustomInput(input)) => {
                    match self.resolve_text_answer(input) {
                        TextResolution::Answer(answer) => return Ok(AskUserResponse::Text(answer)),
                        TextResolution::Invalid(error) => {
                            print_validation_error(&error);
                            continue;
                        }
                        TextResolution::Cancelled => return Ok(AskUserResponse::Cancelled),
                    }
                }
                PanelOutcome::Cancelled => {
                    if self.request.allow_cancel {
                        return Ok(AskUserResponse::Cancelled);
                    }
                }
            }
        }
    }

    fn run_text_input(&self) -> io::Result<AskUserResponse> {
        let default_title = t("shell.ask_user.default_title");
        let title = self
            .request
            .title
            .as_deref()
            .unwrap_or(default_title.as_str());

        let panel = TextInputPanel::new(title.to_string(), self.request);
        match PanelRuntime::new().run(panel).map_err(io::Error::other)? {
            PanelOutcome::Submitted(answer) => Ok(AskUserResponse::Text(answer)),
            PanelOutcome::Cancelled => Ok(AskUserResponse::Cancelled),
        }
    }

    fn resolve_text_answer(&self, input: String) -> TextResolution {
        resolve_text_answer(
            input,
            self.request.default.as_deref(),
            self.request.required,
            self.request.allow_cancel,
            self.request.min_length,
        )
    }
}

struct TextInputPanel<'a> {
    title: String,
    request: &'a AskUserRequest,
    input: String,
    error: Option<String>,
}

impl<'a> TextInputPanel<'a> {
    fn new(title: String, request: &'a AskUserRequest) -> Self {
        Self {
            title,
            request,
            input: String::new(),
            error: None,
        }
    }

    fn submit(&mut self) -> PanelEvent<String> {
        match resolve_text_answer(
            self.input.clone(),
            self.request.default.as_deref(),
            self.request.required,
            self.request.allow_cancel,
            self.request.min_length,
        ) {
            TextResolution::Answer(answer) => PanelEvent::Submit(answer),
            TextResolution::Invalid(error) => {
                self.error = Some(error.message());
                PanelEvent::Continue
            }
            TextResolution::Cancelled => PanelEvent::Cancel,
        }
    }
}

impl PanelComponent for TextInputPanel<'_> {
    type Output = String;

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        9.min(terminal_height.max(1))
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);

        let width = area.width as usize;
        frame.render_widget(
            Paragraph::new("-".repeat(width)).style(Style::default().fg(Color::Cyan)),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(self.title.as_str()).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            chunks[1],
        );
        frame.render_widget(
            Paragraph::new(self.request.prompt.as_str()).style(Style::default().fg(Color::White)),
            chunks[2],
        );

        let hint = if let Some(default) = &self.request.default {
            t_with_args(
                "shell.ask_user.default_hint",
                &std::collections::HashMap::from([("value".to_string(), default.clone())]),
            )
        } else {
            self.request.placeholder.clone().unwrap_or_default()
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
            chunks[3],
        );

        let input = if self.input.is_empty() {
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    self.request.placeholder.as_deref().unwrap_or(""),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::styled(self.input.as_str(), Style::default().fg(Color::White)),
            ])
        };
        frame.render_widget(Paragraph::new(input), chunks[4]);

        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red)),
                chunks[5],
            );
        } else if self.request.allow_cancel {
            frame.render_widget(
                Paragraph::new(t("shell.ask_user.cancel_hint"))
                    .style(Style::default().fg(Color::DarkGray)),
                chunks[5],
            );
        }

        frame.render_widget(
            Paragraph::new(t("shell.ask_user.footer")).style(Style::default().fg(Color::DarkGray)),
            chunks[6],
        );
    }

    fn handle_event(&mut self, event: Event) -> PanelEvent<Self::Output> {
        let Event::Key(key) = event else {
            return PanelEvent::Continue;
        };
        if key.kind == KeyEventKind::Release {
            return PanelEvent::Continue;
        }

        match key.code {
            KeyCode::Esc => {
                if self.request.allow_cancel {
                    PanelEvent::Cancel
                } else {
                    PanelEvent::Continue
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.request.allow_cancel {
                    PanelEvent::Cancel
                } else {
                    PanelEvent::Continue
                }
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input.pop();
                self.error = None;
                PanelEvent::Continue
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.push(ch);
                self.error = None;
                PanelEvent::Continue
            }
            _ => PanelEvent::Continue,
        }
    }
}

fn resolve_text_answer(
    input: String,
    default: Option<&str>,
    required: bool,
    allow_cancel: bool,
    min_length: usize,
) -> TextResolution {
    let input = input.trim().to_string();

    let answer = if input.is_empty() {
        if let Some(default) = default {
            default.to_string()
        } else if !required {
            return TextResolution::Answer(String::new());
        } else if allow_cancel {
            return TextResolution::Cancelled;
        } else {
            return TextResolution::Invalid(TextValidationError::Required);
        }
    } else {
        input
    };

    if answer.len() < min_length {
        return TextResolution::Invalid(TextValidationError::MinLength(min_length));
    }

    TextResolution::Answer(answer)
}

fn print_validation_error(error: &TextValidationError) {
    println!("{}", crate::theme::error(&error.message()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_uses_default() {
        assert_eq!(
            resolve_text_answer("".to_string(), Some("default"), true, true, 0),
            TextResolution::Answer("default".to_string())
        );
    }

    #[test]
    fn empty_text_allowed_when_not_required() {
        assert_eq!(
            resolve_text_answer("".to_string(), None, false, true, 0),
            TextResolution::Answer(String::new())
        );
    }

    #[test]
    fn empty_required_text_cancels_when_allowed() {
        assert_eq!(
            resolve_text_answer("".to_string(), None, true, true, 0),
            TextResolution::Cancelled
        );
    }

    #[test]
    fn empty_required_text_retries_when_cancel_is_disabled() {
        assert_eq!(
            resolve_text_answer("".to_string(), None, true, false, 0),
            TextResolution::Invalid(TextValidationError::Required)
        );
    }

    #[test]
    fn short_text_retries() {
        assert_eq!(
            resolve_text_answer("ab".to_string(), None, true, true, 3),
            TextResolution::Invalid(TextValidationError::MinLength(3))
        );
    }

    #[test]
    fn default_that_is_too_short_returns_min_length_error() {
        assert_eq!(
            resolve_text_answer("".to_string(), Some("ok"), true, true, 3),
            TextResolution::Invalid(TextValidationError::MinLength(3))
        );
    }

    #[test]
    fn text_is_trimmed() {
        assert_eq!(
            resolve_text_answer(" answer \n".to_string(), None, true, true, 0),
            TextResolution::Answer("answer".to_string())
        );
    }
}
