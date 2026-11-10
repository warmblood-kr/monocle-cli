use crate::agent::error;
use crate::types::*;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// JSON Lines file storage. A successful receipt acknowledges serialization,
/// write and `flush` only; it does not promise fsync or durable storage. A
/// failed append can leave a partial line. The adapter does not retry.
pub struct FileAgentStorage {
    path: PathBuf,
}

impl FileAgentStorage {
    /// Configure the journal path without opening or creating it.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AgentStorage for FileAgentStorage {
    fn append(&mut self, record: &TurnRecord) -> Result<StorageReceipt, AgentError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| error(AgentPhase::Storage, err.to_string()))?;
        let mut writer = BufWriter::new(file);
        let json_record = serde_json::json!({
            "turn_id": &record.turn_id,
            "response": &record.response,
        });
        serde_json::to_writer(&mut writer, &json_record)
            .map_err(|err| error(AgentPhase::Storage, err.to_string()))?;
        writer
            .write_all(b"\n")
            .map_err(|err| error(AgentPhase::Storage, err.to_string()))?;
        writer
            .flush()
            .map_err(|err| error(AgentPhase::Storage, err.to_string()))?;
        Ok(StorageReceipt {
            reference: self.path.to_string_lossy().into_owned(),
        })
    }
}

/// Adapter around a host-owned transactional writer. The host callback owns
/// transaction semantics; this adapter makes no live database or durability
/// claim and does not retry.
pub struct DbAgentStorage<F> {
    write_transaction: F,
}

impl<F> DbAgentStorage<F> {
    /// Store the writer callback without invoking it.
    pub fn new(write_transaction: F) -> Self {
        Self { write_transaction }
    }
}

impl<F> AgentStorage for DbAgentStorage<F>
where
    F: FnMut(&TurnRecord) -> Result<StorageReceipt, AgentError>,
{
    fn append(&mut self, record: &TurnRecord) -> Result<StorageReceipt, AgentError> {
        (self.write_transaction)(record)
    }
}
