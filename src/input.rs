use anyhow::{Context, Result, bail};
use gix::ObjectId;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Immutable Git blob identity and inspected size before its body is requested.
#[derive(Clone, Copy)]
pub struct GitBlob {
    pub object: ObjectId,
    pub bytes: u64,
}

impl GitBlob {
    /// Read the inspected object while preserving its pinned-size invariant.
    pub fn read(self, repo: &gix::Repository) -> Result<Vec<u8>> {
        let blob = repo.find_blob(self.object);
        let mut blob = blob.context("failed to read Git blob")?;
        if u64::try_from(blob.data.len()).unwrap_or(u64::MAX) != self.bytes {
            bail!(
                "Git returned {} bytes for a blob reported as {} bytes",
                blob.data.len(),
                self.bytes
            );
        }

        Ok(std::mem::take(&mut blob.data))
    }
}

/// Open file plus the size observed from its handle before any allocation.
pub struct OpenFile {
    file: File,
    path: PathBuf,
    bytes: u64,
}

impl OpenFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path);
        let file = file.with_context(|| format!("failed to open file {}", path.display()))?;
        let metadata = file.metadata();
        let metadata =
            metadata.with_context(|| format!("failed to inspect file {}", path.display()))?;

        Ok(Self {
            file,
            path: path.to_owned(),
            bytes: metadata.len(),
        })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Bounded read that catches a file growing after its initial `fstat`.
    pub fn read(self, limit: u64) -> Result<BoundedBytes> {
        if self.bytes > limit {
            return Ok(BoundedBytes::TooLarge(self.bytes));
        }

        let capacity = usize::try_from(self.bytes).with_context(|| {
            format!(
                "file is too large for this platform: {}",
                self.path.display()
            )
        })?;
        let mut source = Vec::with_capacity(capacity);
        let mut reader = self.file.take(limit.saturating_add(1));
        let read = reader.read_to_end(&mut source);
        read.with_context(|| format!("failed to read file {}", self.path.display()))?;
        if u64::try_from(source.len()).unwrap_or(u64::MAX) <= limit {
            return Ok(BoundedBytes::Contents(source));
        }

        let metadata = reader.get_ref().metadata();
        let metadata = metadata
            .with_context(|| format!("failed to reinspect file {}", self.path.display()))?;
        let bytes_read = u64::try_from(source.len()).unwrap_or(u64::MAX);
        let bytes = metadata.len().max(bytes_read);
        Ok(BoundedBytes::TooLarge(bytes))
    }
}

pub enum BoundedBytes {
    Contents(Vec<u8>),
    TooLarge(u64),
}

/// Decode terminal-review text while leaving binary content outside the review.
pub fn decode_text(source: Vec<u8>) -> Option<String> {
    if source.contains(&0) {
        return None;
    }

    String::from_utf8(source).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn read_limit_catches_growth_after_the_handle_was_measured() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("growing.txt");
        fs::write(&path, b"1234").expect("write initial input");
        let input = OpenFile::open(&path).expect("open initial input");
        fs::write(&path, b"12345").expect("grow input");

        assert!(matches!(input.read(4), Ok(BoundedBytes::TooLarge(5))));
    }
}
