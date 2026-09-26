//! A native conversation may only be resumed from a completed, matching Chaos
//! prefix. Consume the checkpoint before dispatch: a crash or failed turn must
//! not replay input against a provider transcript that may already have advanced.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use super::tools::render_clamp_response_item;
use crate::client_common::Prompt;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub(super) enum Backend {
    #[default]
    Claude,
    Antigravity,
}

impl Backend {
    fn valid_id(self, id: &str) -> bool {
        match self {
            Self::Claude => uuid::Uuid::parse_str(id).is_ok(),
            Self::Antigravity => {
                !id.is_empty()
                    && id.len() <= 256
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Checkpoint {
    version: u8,
    #[serde(default)]
    backend: Backend,
    session_id: String,
    model: String,
    system: String,
    cwd: PathBuf,
    input_len: usize,
    prefix: String,
}

#[derive(Debug)]
pub(super) struct NativeResume {
    path: Option<PathBuf>,
    memory: Mutex<Option<Checkpoint>>,
}

fn fingerprint(items: &[String]) -> String {
    let mut hash = Sha256::new();
    for item in items {
        hash.update((item.len() as u64).to_le_bytes());
        hash.update(item.as_bytes());
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn rendered_input(prompt: &Prompt) -> Vec<String> {
    prompt
        .get_formatted_input()
        .iter()
        .filter_map(render_clamp_response_item)
        .collect()
}

impl Checkpoint {
    pub(super) fn completed(
        backend: Backend,
        session_id: &str,
        model: &str,
        system: &str,
        cwd: PathBuf,
        mut input: Vec<String>,
        output: &str,
    ) -> Option<Self> {
        if !backend.valid_id(session_id) {
            return None;
        }
        input.push(format!(
            "<message role=\"assistant\">\n{output}\n</message>"
        ));
        Some(Self {
            version: 1,
            backend,
            session_id: session_id.to_string(),
            model: model.to_string(),
            system: fingerprint(&[system.to_string()]),
            cwd,
            input_len: input.len(),
            prefix: fingerprint(&input),
        })
    }

    pub(super) fn continuation(
        &self,
        backend: Backend,
        model: &str,
        system: &str,
        cwd: &std::path::Path,
        input: &[String],
    ) -> Option<(String, String)> {
        if self.version != 1
            || self.backend != backend
            || self.model != model
            || self.system != fingerprint(&[system.to_string()])
            || self.cwd != cwd
            || self.input_len >= input.len()
            || !backend.valid_id(&self.session_id)
            || self.prefix != fingerprint(&input[..self.input_len])
        {
            return None;
        }
        // Include *all* unsent items, including new developer instructions and
        // hook messages. Merely selecting the last user message drops context.
        Some((
            self.session_id.clone(),
            input[self.input_len..].join("\n\n"),
        ))
    }
}

impl NativeResume {
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            memory: Mutex::new(None),
        }
    }

    pub(super) fn take(&self) -> std::io::Result<Option<Checkpoint>> {
        let Some(path) = &self.path else {
            return Ok(self
                .memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take());
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // Failure to invalidate is fatal before dispatch, not a warning: leaving
        // an old checkpoint behind could duplicate actions on the next resume.
        self.clear()?;
        match serde_json::from_slice(&bytes) {
            Ok(checkpoint) => Ok(Some(checkpoint)),
            Err(error) => {
                tracing::warn!("ignoring invalid native resume checkpoint: {error}");
                Ok(None)
            }
        }
    }

    pub(super) fn save(&self, checkpoint: Checkpoint) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            *self
                .memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(checkpoint);
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("checkpoint has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut file, &checkpoint)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    pub(super) fn clear(&self) -> std::io::Result<()> {
        self.memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(path) = &self.path {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
