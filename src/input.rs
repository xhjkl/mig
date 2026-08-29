use anyhow::{Context, Result};
use gix::ObjectId;
use std::fs::File;
use std::io::{Read, Take};
use std::path::{Path, PathBuf};

/// Immutable Git blob identity and inspected size before its body is requested.
#[derive(Clone, Copy)]
pub(crate) struct GitBlob {
    pub(crate) object: ObjectId,
    pub(crate) bytes: u64,
}

/// Open file plus the size observed from its handle before any allocation.
pub(crate) struct OpenFile {
    file: File,
    path: PathBuf,
    bytes: u64,
}

impl OpenFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
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

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Bounded read that catches a file growing after its initial `fstat`.
    pub(crate) fn read(self, limit: u64) -> Result<BoundedBytes> {
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

        let bytes = final_size(&reader, source.len(), &self.path)?;
        Ok(BoundedBytes::TooLarge(bytes))
    }
}

fn final_size(reader: &Take<File>, bytes_read: usize, path: &Path) -> Result<u64> {
    let metadata = reader.get_ref().metadata();
    let metadata =
        metadata.with_context(|| format!("failed to reinspect file {}", path.display()))?;
    let bytes_read = u64::try_from(bytes_read).unwrap_or(u64::MAX);
    Ok(metadata.len().max(bytes_read))
}

pub(crate) enum BoundedBytes {
    Contents(Vec<u8>),
    TooLarge(u64),
}

/// Decode terminal-review text while leaving binary content outside the review.
pub(crate) fn decode_text(source: Vec<u8>) -> Option<String> {
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
    fn exact_limit_is_read_and_one_byte_over_is_deferred() {
        let directory = TempDir::new().expect("temporary directory");
        let exact = directory.path().join("exact.txt");
        let over = directory.path().join("over.txt");
        fs::write(&exact, b"1234").expect("write exact input");
        fs::write(&over, b"12345").expect("write oversized input");

        let exact = OpenFile::open(&exact).expect("open exact input");
        let over = OpenFile::open(&over).expect("open oversized input");

        assert!(matches!(exact.read(4), Ok(BoundedBytes::Contents(source)) if source == b"1234"));
        assert!(matches!(over.read(4), Ok(BoundedBytes::TooLarge(5))));
    }

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
