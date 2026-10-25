//! What a step says it did.

/// What one step did, in words, for the processing log (#85).
///
/// Every step returns one, including when it changed nothing, so the log
/// says why. The dispatcher ([`crate::gpr::GPR::process`]) times the step
/// and writes the report as one line of the log; see
/// [`crate::gpr::processing_log`] for the format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    name: String,
    event: String,
    warning: bool,
}

impl StepReport {
    /// A step that did what it was asked.
    ///
    /// `name` starts the log line. It is the step's name, or a longer form
    /// of it when the arguments change what ran (`zero_corr(coppens,
    /// trace)`).
    pub fn new(name: impl Into<String>, event: impl Into<String>) -> Self {
        StepReport {
            name: name.into(),
            event: event.into(),
            warning: false,
        }
    }

    /// A step that met data it could not handle as asked and fell back,
    /// usually to changing nothing, rather than failing: a bad file should
    /// not stop a batch. Bad *arguments* are an error instead.
    ///
    /// The log says `Warning: {event}`.
    pub fn warning(name: impl Into<String>, event: impl AsRef<str>) -> Self {
        StepReport {
            name: name.into(),
            event: format!("Warning: {}", event.as_ref()),
            warning: true,
        }
    }

    /// Flag a report as a warning when `warned`, for a step whose event
    /// says what went wrong further in than its start, such as a note after
    /// what was done.
    pub fn warn_if(mut self, warned: bool) -> Self {
        self.warning |= warned;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn event(&self) -> &str {
        &self.event
    }

    pub fn is_warning(&self) -> bool {
        self.warning
    }
}
