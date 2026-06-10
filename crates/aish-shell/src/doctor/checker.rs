use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub checker: String,
    pub items: Vec<CheckItem>,
    pub status: CheckStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub fixable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl CheckItem {
    pub fn pass(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Pass,
            message: message.into(),
            fixable: false,
            hint: None,
        }
    }

    pub fn warn(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            message: message.into(),
            fixable: false,
            hint: None,
        }
    }

    pub fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            message: message.into(),
            fixable: false,
            hint: None,
        }
    }

    pub fn fixable(mut self) -> Self {
        self.fixable = true;
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl CheckResult {
    pub fn from_items(checker: impl Into<String>, items: Vec<CheckItem>) -> Self {
        let status = if items.iter().any(|i| i.status == CheckStatus::Fail) {
            CheckStatus::Fail
        } else if items.iter().any(|i| i.status == CheckStatus::Warn) {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        };
        Self {
            checker: checker.into(),
            items,
            status,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FixResult {
    pub success: bool,
    pub message: String,
}

pub trait Checker: Send + Sync {
    fn name(&self) -> &str;
    fn check(&self) -> Vec<CheckResult>;
    fn fix(&self, result: &CheckItem) -> FixResult;
    fn box_clone(&self) -> Box<dyn Checker>;
}
