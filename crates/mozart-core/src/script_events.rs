//! Script event name constants.
//!
//! Mirrors `Composer\Script\ScriptEvents` (user-runnable script events) and the
//! internal event constants from `Composer\Installer\PackageEvents`,
//! `Composer\Installer\InstallerEvents`, and `Composer\Plugin\PluginEvents`
//! that are referenced by the dispatcher when validating run-script input.

// User-runnable script events — `Composer\Script\ScriptEvents`.
pub const PRE_INSTALL_CMD: &str = "pre-install-cmd";
pub const POST_INSTALL_CMD: &str = "post-install-cmd";
pub const PRE_UPDATE_CMD: &str = "pre-update-cmd";
pub const POST_UPDATE_CMD: &str = "post-update-cmd";
pub const PRE_STATUS_CMD: &str = "pre-status-cmd";
pub const POST_STATUS_CMD: &str = "post-status-cmd";
pub const POST_ROOT_PACKAGE_INSTALL: &str = "post-root-package-install";
pub const POST_CREATE_PROJECT_CMD: &str = "post-create-project-cmd";
pub const PRE_ARCHIVE_CMD: &str = "pre-archive-cmd";
pub const POST_ARCHIVE_CMD: &str = "post-archive-cmd";
pub const PRE_AUTOLOAD_DUMP: &str = "pre-autoload-dump";
pub const POST_AUTOLOAD_DUMP: &str = "post-autoload-dump";

// Internal events — `Composer\Installer\PackageEvents`.
pub const PRE_PACKAGE_INSTALL: &str = "pre-package-install";
pub const POST_PACKAGE_INSTALL: &str = "post-package-install";
pub const PRE_PACKAGE_UPDATE: &str = "pre-package-update";
pub const POST_PACKAGE_UPDATE: &str = "post-package-update";
pub const PRE_PACKAGE_UNINSTALL: &str = "pre-package-uninstall";
pub const POST_PACKAGE_UNINSTALL: &str = "post-package-uninstall";

// Internal events — `Composer\Installer\InstallerEvents`.
pub const PRE_OPERATIONS_EXEC: &str = "pre-operations-exec";

// Internal events — `Composer\Plugin\PluginEvents`.
pub const INIT: &str = "init";
pub const COMMAND: &str = "command";
pub const PRE_FILE_DOWNLOAD: &str = "pre-file-download";
pub const POST_FILE_DOWNLOAD: &str = "post-file-download";
pub const PRE_COMMAND_RUN: &str = "pre-command-run";
pub const PRE_POOL_CREATE: &str = "pre-pool-create";

/// Script events the user is allowed to invoke via `run-script`.
///
/// Mirrors `RunScriptCommand::$scriptEvents` in Composer.
pub const USER_RUNNABLE: &[&str] = &[
    PRE_INSTALL_CMD,
    POST_INSTALL_CMD,
    PRE_UPDATE_CMD,
    POST_UPDATE_CMD,
    PRE_STATUS_CMD,
    POST_STATUS_CMD,
    POST_ROOT_PACKAGE_INSTALL,
    POST_CREATE_PROJECT_CMD,
    PRE_ARCHIVE_CMD,
    POST_ARCHIVE_CMD,
    PRE_AUTOLOAD_DUMP,
    POST_AUTOLOAD_DUMP,
];

/// All recognised event names — user-runnable plus internal events emitted by
/// the dispatcher during install/update/etc. Used by `run-script` to surface a
/// "cannot be run with this command" error for known internal events.
pub const ALL: &[&str] = &[
    PRE_INSTALL_CMD,
    POST_INSTALL_CMD,
    PRE_UPDATE_CMD,
    POST_UPDATE_CMD,
    PRE_STATUS_CMD,
    POST_STATUS_CMD,
    POST_ROOT_PACKAGE_INSTALL,
    POST_CREATE_PROJECT_CMD,
    PRE_ARCHIVE_CMD,
    POST_ARCHIVE_CMD,
    PRE_AUTOLOAD_DUMP,
    POST_AUTOLOAD_DUMP,
    PRE_PACKAGE_INSTALL,
    POST_PACKAGE_INSTALL,
    PRE_PACKAGE_UPDATE,
    POST_PACKAGE_UPDATE,
    PRE_PACKAGE_UNINSTALL,
    POST_PACKAGE_UNINSTALL,
    PRE_OPERATIONS_EXEC,
    INIT,
    COMMAND,
    PRE_FILE_DOWNLOAD,
    POST_FILE_DOWNLOAD,
    PRE_COMMAND_RUN,
    PRE_POOL_CREATE,
];
