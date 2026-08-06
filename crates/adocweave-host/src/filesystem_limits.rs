/// Bounds applied while the host discovers and reads filesystem resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemReadLimits {
    /// Maximum number of filesystem resources charged to one session.
    pub max_files: usize,
    /// Maximum combined bytes charged to one session.
    pub max_total_bytes: u64,
    /// Maximum bytes read from one filesystem resource.
    pub max_resource_bytes: u64,
}

impl Default for FilesystemReadLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_total_bytes: 50 * 1024 * 1024,
            max_resource_bytes: 10 * 1024 * 1024,
        }
    }
}
