use std::path::Path;

use crate::filesystem_limits::FilesystemReadLimits;

use super::ResourceError;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceBudget {
    files: usize,
    bytes: u64,
}

impl ResourceBudget {
    pub fn charge(
        &mut self,
        path: &Path,
        bytes: u64,
        limits: FilesystemReadLimits,
    ) -> Result<(), ResourceError> {
        if bytes > limits.max_resource_bytes {
            return Err(ResourceError::ResourceTooLarge(path.to_owned()));
        }
        let files = self.files.checked_add(1).ok_or(ResourceError::FileLimit {
            limit: limits.max_files,
        })?;
        if files > limits.max_files {
            return Err(ResourceError::FileLimit {
                limit: limits.max_files,
            });
        }
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(ResourceError::ByteLimit)?;
        if total > limits.max_total_bytes {
            return Err(ResourceError::ByteLimit);
        }
        self.files = files;
        self.bytes = total;
        Ok(())
    }

    pub(super) fn replace(
        &mut self,
        path: &Path,
        previous: Option<u64>,
        bytes: u64,
        limits: FilesystemReadLimits,
    ) -> Result<(), ResourceError> {
        let Some(previous) = previous else {
            return self.charge(path, bytes, limits);
        };
        if bytes > limits.max_resource_bytes {
            return Err(ResourceError::ResourceTooLarge(path.to_owned()));
        }
        let retained = self
            .bytes
            .checked_sub(previous)
            .expect("charged bytes are part of the total");
        let total = retained
            .checked_add(bytes)
            .ok_or(ResourceError::ByteLimit)?;
        if total > limits.max_total_bytes {
            return Err(ResourceError::ByteLimit);
        }
        self.bytes = total;
        Ok(())
    }

    pub(super) fn release(&mut self, bytes: u64) {
        self.files = self
            .files
            .checked_sub(1)
            .expect("released file was charged");
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .expect("released bytes were charged");
    }

    pub const fn files(self) -> usize {
        self.files
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}
