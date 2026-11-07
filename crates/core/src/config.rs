//! Runtime configuration, independent of settings persistence.
use crate::domain::OutputDir;

/// The shared location for models, tools and temporary media files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CacheLocation {
    #[default]
    UserCache,
    Custom(OutputDir),
}

impl CacheLocation {
    /// Resolves the configured model/cache store without creating directories.
    /// # Errors
    /// Returns an error if the platform has no default cache location.
    pub fn store(&self) -> Result<crate::model::ModelStore, crate::model::ModelStoreError> {
        match self {
            Self::UserCache => crate::model::ModelStore::in_user_cache(),
            Self::Custom(directory) => Ok(crate::model::ModelStore::new(directory.path())),
        }
    }
}
