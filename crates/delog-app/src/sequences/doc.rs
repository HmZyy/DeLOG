use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Parser,
    Script,
    Dataflow,
    Layout,
}

impl StepKind {
    pub const ALL: [Self; 4] = [Self::Parser, Self::Script, Self::Dataflow, Self::Layout];
    pub fn label(self) -> &'static str {
        match self {
            Self::Parser => "Parser",
            Self::Script => "Script",
            Self::Dataflow => "Dataflow",
            Self::Layout => "Layout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceStep {
    pub id: u64,
    pub kind: StepKind,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceDoc {
    pub delog_sequence: u32,
    pub id: String,
    pub name: String,
    pub next_step_id: u64,
    pub steps: Vec<SequenceStep>,
}

impl SequenceDoc {
    pub fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            delog_sequence: 1,
            id: format!("{stamp:x}-{:x}-{counter:x}", std::process::id()),
            name: name.into(),
            next_step_id: 1,
            steps: Vec::new(),
        }
    }
    pub fn push(&mut self, kind: StepKind, reference: &str) -> u64 {
        let id = self.next_step_id;
        self.next_step_id += 1;
        self.steps.push(SequenceStep {
            id,
            kind,
            reference: reference.into(),
        });
        id
    }
    pub fn move_step(&mut self, from: usize, to: usize) -> Result<(), String> {
        if from >= self.steps.len() || to >= self.steps.len() {
            return Err("step index is out of range".into());
        }
        let step = self.steps.remove(from);
        self.steps.insert(to, step);
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.delog_sequence != 1 {
            return Err("unsupported sequence version".into());
        }
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err("invalid sequence identity".into());
        }
        let mut ids = std::collections::HashSet::new();
        for step in &self.steps {
            if step.id == 0 || step.id >= self.next_step_id || !ids.insert(step.id) {
                return Err("invalid or duplicate step identity".into());
            }
            super::store::validate_name(&step.reference)?;
        }
        if self.next_step_id == 0 || self.next_step_id == u64::MAX {
            return Err("sequence step limit reached".into());
        }
        super::store::validate_name(&self.name)
    }
}
