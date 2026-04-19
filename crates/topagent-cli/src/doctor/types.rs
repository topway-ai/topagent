#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckLevel {
    Ok,
    Warning,
    Error,
}

impl CheckLevel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }
}

pub(crate) struct CheckResult {
    pub(crate) name: &'static str,
    pub(crate) level: CheckLevel,
    pub(crate) detail: String,
    pub(crate) hint: Option<String>,
}
