//! Sending a file from one box to another ([`FileBoxes::transfer`]).
//!
//! Only Melissa does this, never an agent's sandbox: the owner from the details panel or the
//! API, or the lead agent's `transfer_file` tool once the owner said yes. The source is opened
//! like any served file (by id, never through a symlink); the copy is a new file in the
//! receiving box with a new id, counted as added now (retention starts again there), by
//! [`AddedBy::Sent`] with the box it came from. The receiving box's limits apply
//! ([`MAX_BOX_BYTES`], [`MAX_BOX_FILES`]). A move removes the source after the copy is safely
//! written. Audited `files.transferred` (who, from, to, file, size, copy or move).

use std::{collections::HashSet, fs, io};

use serde::Serialize;

use super::{
    head_of, lock, modified_of, open_in, random_id, unique_name, AddedBy, BoxFile, FileBoxError,
    FileBoxes, MAX_BOX_BYTES, MAX_BOX_FILES,
};
use crate::{attachments::sniff, tools::permissions::Principal};

/// Copy (the default: both boxes keep it) or move (the source loses it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    #[default]
    Copy,
    Move,
}

impl Transfer {
    /// `copied` or `moved`.
    pub fn past(self) -> &'static str {
        match self {
            Self::Copy => "copied",
            Self::Move => "moved",
        }
    }
}

/// What a transfer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transferred {
    /// The file as it was in the sending box.
    pub source: BoxFile,
    /// The new file in the receiving box (its name may differ: `report-2.md`).
    pub file: BoxFile,
    pub mode: Transfer,
}

impl FileBoxes {
    /// Send file `id` of `from`'s box to `to`'s box (`actor`: `ui` for the owner, `lead` for
    /// the lead agent's approved tool call). Refused for the same box, an unknown file, or a
    /// receiving box that is full ([`FileBoxError::Full`] with the receiving key).
    pub fn transfer(
        &self,
        from: &Principal,
        id: &str,
        to: &Principal,
        mode: Transfer,
        actor: &str,
    ) -> Result<Transferred, FileBoxError> {
        let from_key = Self::key(from)?;
        let to_key = Self::key(to)?;
        if from_key == to_key {
            return Err(FileBoxError::Invalid("it is already in that box".into()));
        }
        let from_dir = self.dir(from).ok_or(FileBoxError::BadOwner)?;
        self.sync(from);
        let source = self.get(from, id).ok_or(FileBoxError::NotFound)?;
        let mut src = open_in(&from_dir, &source.name)?;
        let size = src.metadata()?.len();

        self.sync(to);
        let to_dir = self.ensure_dir(to)?;
        let now = self.now();
        // Checked and the name taken (the new, empty file) under the lock; copied without it.
        let (name, out) = {
            let mut boxes = lock(&self.inner.boxes);
            let index = self.index(&mut boxes, &to_key);
            let used: u64 = index.iter().map(|f| f.size).sum();
            if index.len() >= MAX_BOX_FILES || used.saturating_add(size) > MAX_BOX_BYTES {
                return Err(FileBoxError::Full(to_key));
            }
            let taken: HashSet<String> = index.iter().map(|f| f.name.clone()).collect();
            let name = unique_name(&to_dir, &source.name, &taken);
            let out = create_new(&to_dir.join(&name))?;
            (name, out)
        };
        let path = to_dir.join(&name);
        if let Err(e) = copy_into(&mut src, out) {
            let _ = fs::remove_file(&path);
            return Err(e.into());
        }
        let mime = match open_in(&to_dir, &name) {
            Ok(mut f) => sniff(&head_of(&mut f)).0.to_string(),
            Err(_) => source.mime.clone(),
        };
        let file = BoxFile {
            id: random_id()?,
            name,
            mime,
            size,
            added: now,
            by: AddedBy::Sent,
            modified: modified_of(&path),
            from: Some(from_key.clone()),
        };
        {
            let mut boxes = lock(&self.inner.boxes);
            let index = self.index(&mut boxes, &to_key);
            index.retain(|f| f.name != file.name);
            index.push(file.clone());
            let snapshot = index.clone();
            drop(boxes);
            self.save_index(&to_key, &snapshot);
        }
        if mode == Transfer::Move {
            // `remove_file` on the name removes that entry only (never what a link points at).
            match fs::remove_file(from_dir.join(&source.name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(file = %source.name, "moving a file: removing the original: {e}")
                }
            }
            let mut boxes = lock(&self.inner.boxes);
            let index = self.index(&mut boxes, &from_key);
            index.retain(|f| f.id != source.id);
            let snapshot = index.clone();
            drop(boxes);
            self.save_index(&from_key, &snapshot);
        }
        self.audit(
            "files.transferred",
            actor,
            &format!(
                "{} {} ({size} bytes) from {from_key}'s box to {to_key}'s box{}",
                mode.past(),
                source.name,
                if file.name == source.name {
                    String::new()
                } else {
                    format!(" as {}", file.name)
                }
            ),
        );
        self.changed(to);
        if mode == Transfer::Move {
            self.changed(from);
        }
        Ok(Transferred { source, file, mode })
    }
}

/// A new file at `path` (mode 0600; `create_new` never follows a symlink).
fn create_new(path: &std::path::Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn copy_into(src: &mut fs::File, mut out: fs::File) -> io::Result<()> {
    io::copy(src, &mut out)?;
    out.sync_all()
}
