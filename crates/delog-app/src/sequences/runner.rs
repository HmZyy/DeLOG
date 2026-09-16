use super::doc::{SequenceDoc, SequenceStep};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepToken {
    pub run: u64,
    pub step: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone)]
pub enum StepOutcome {
    Succeeded,
    Failed(String),
    Cancelled,
}
#[derive(Debug, Clone)]
pub struct StepRequest {
    pub token: StepToken,
    pub step: SequenceStep,
}

pub struct SequenceRunner {
    pub doc: SequenceDoc,
    pub status: RunStatus,
    pub states: Vec<StepStatus>,
    pub error: Option<String>,
    pub cursor: usize,
    run: u64,
    active: Option<StepToken>,
}
impl SequenceRunner {
    pub fn start(doc: SequenceDoc, run: u64) -> Self {
        let status = if doc.steps.is_empty() {
            RunStatus::Succeeded
        } else {
            RunStatus::Running
        };
        let states = vec![StepStatus::Pending; doc.steps.len()];
        Self {
            doc,
            status,
            states,
            error: None,
            cursor: 0,
            run,
            active: None,
        }
    }
    pub fn take_request(&mut self) -> Option<StepRequest> {
        if self.status != RunStatus::Running || self.active.is_some() {
            return None;
        }
        let step = self.doc.steps.get(self.cursor)?.clone();
        let token = StepToken {
            run: self.run,
            step: step.id,
        };
        self.active = Some(token);
        self.states[self.cursor] = StepStatus::Running;
        Some(StepRequest { token, step })
    }
    pub fn accepts(&self, token: StepToken) -> bool {
        self.active == Some(token) && self.status == RunStatus::Running
    }

    pub fn complete(&mut self, token: StepToken, outcome: StepOutcome) {
        if self.active != Some(token) || self.status != RunStatus::Running {
            return;
        }
        self.active = None;
        match outcome {
            StepOutcome::Succeeded => {
                self.states[self.cursor] = StepStatus::Succeeded;
                self.cursor += 1;
                if self.cursor == self.doc.steps.len() {
                    self.status = RunStatus::Succeeded;
                }
            }
            StepOutcome::Failed(error) => {
                self.states[self.cursor] = StepStatus::Failed;
                self.error = Some(error);
                self.status = RunStatus::Failed;
            }
            StepOutcome::Cancelled => {
                self.states[self.cursor] = StepStatus::Pending;
                self.status = RunStatus::Cancelled;
            }
        }
    }
    pub fn cancel(&mut self) {
        if self.status == RunStatus::Running {
            self.status = RunStatus::Cancelled;
            self.active = None;
        }
    }
}
