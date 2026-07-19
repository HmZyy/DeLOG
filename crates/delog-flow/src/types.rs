use std::sync::Arc;

use delog_core::identity::TopicId;

use crate::graph::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineId {
    Topic(TopicId),
    Node(NodeId),
    NodeOutput(NodeId, u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalMeta {
    pub timeline: TimelineId,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub t: Arc<Vec<i64>>,
    pub v: Arc<Vec<f64>>,
    pub meta: SignalMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Signal(Signal),
    Scalar(f64),
}
