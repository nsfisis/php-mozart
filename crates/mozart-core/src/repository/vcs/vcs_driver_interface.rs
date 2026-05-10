use crate::repository::vcs::{DistReference, SourceReference};

/// The VCS driver interface.
///
/// Corresponds to Composer's `VcsDriverInterface`.
#[allow(async_fn_in_trait)]
pub trait VcsDriverInterface {
    /// Initialize the driver (e.g., clone mirror, fetch API metadata).
    async fn initialize(&mut self) -> anyhow::Result<()>;

    /// The root identifier (default branch/trunk).
    fn root_identifier(&self) -> &str;

    /// All branches as `name -> commit_hash`.
    async fn branches(&mut self) -> anyhow::Result<&std::collections::BTreeMap<String, String>>;

    /// All tags as `name -> commit_hash`.
    async fn tags(&mut self) -> anyhow::Result<&std::collections::BTreeMap<String, String>>;

    /// Get composer.json content parsed as JSON for a given identifier.
    async fn composer_information(
        &mut self,
        identifier: &str,
    ) -> anyhow::Result<Option<serde_json::Value>>;

    /// Get raw file content at a given path and identifier.
    async fn file_content(&self, file: &str, identifier: &str) -> anyhow::Result<Option<String>>;

    /// Get the change date for a given identifier (ISO 8601).
    async fn change_date(&self, identifier: &str) -> anyhow::Result<Option<String>>;

    /// Get the dist reference for a given identifier.
    async fn dist(&self, identifier: &str) -> anyhow::Result<Option<DistReference>>;

    /// Get the source reference for a given identifier.
    fn source(&self, identifier: &str) -> SourceReference;

    /// The canonical URL of this repository.
    fn url(&self) -> &str;

    /// Clean up resources (temp dirs, etc.).
    async fn cleanup(&mut self) -> anyhow::Result<()>;
}
