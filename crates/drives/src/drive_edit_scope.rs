//! Frame membership captured atomically with the first edit in a drive queue.
use crate::grouper::DriveClipWindow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub runs: Vec<u32>,
    pub id: Option<String>,
    pub key: Option<String>,
    pub uploaded_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub version: u8,
    pub windows: Vec<DriveClipWindow>,
    pub sources: BTreeMap<String, Source>,
}

impl Scope {
    pub fn matches(&self, current: &Self) -> bool {
        self.version == 1 && current.version == 1 && self.windows == current.windows
            && self.sources.len() == current.sources.len()
            && self.sources.iter().all(|(file, old)| current.sources.get(file).is_some_and(|new| {
                old.runs == new.runs
                    // The first upload may happen after an offline tag edit.
                    // Once assigned, the recording identity cannot change.
                    && old.id.as_ref().is_none_or(|id| new.id.as_ref() == Some(id))
                    && old.key.as_ref().is_none_or(|key| new.key.as_ref() == Some(key))
                    && old.uploaded_at.filter(|at| *at > 0).is_none_or(|at| new.uploaded_at == Some(at))
            }))
    }
}
