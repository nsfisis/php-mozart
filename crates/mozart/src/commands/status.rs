use crate::composer::Composer;
use clap::Args;
use indexmap::IndexMap;
use mozart_core::composer::{InstallationSource, LocalPackage};
use mozart_core::console::Console;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::exit_code;
use mozart_vcs::version_guesser::VersionGuesser;

#[derive(Args)]
pub struct StatusArgs {}

struct VcsVerChange {
    previous: VerRef,
    current: VerRef,
}

struct VerRef {
    version: String,
    reference: String,
}

pub async fn execute(
    _args: &StatusArgs,
    cli: &super::Cli,
    console: &Console,
) -> anyhow::Result<()> {
    let composer = Composer::require(cli.working_dir()?)?;

    let installed_repo = composer.repository_manager().local_repository();

    let dm = composer.download_manager();
    let im = composer.installation_manager();

    let mut errors = IndexMap::new();
    let mut unpushed_changes = IndexMap::new();
    let mut vcs_version_changes = IndexMap::new();

    let guesser = VersionGuesser::new();

    for package in installed_repo.get_canonical_packages() {
        let Some(downloader) = dm.get_downloader_for_package(package) else {
            continue;
        };
        let Some(target_dir) = im.get_install_path(package) else {
            continue;
        };
        let target_key = target_dir.display().to_string();

        // ChangeReportInterface — Composer mirrors the symlink branch and
        // the local-changes branch unconditionally; the latter overrides
        // the former when both fire.
        if std::fs::symlink_metadata(&target_dir)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            errors.insert(
                target_key.clone(),
                format!("{target_key} is a symbolic link."),
            );
        }
        if let Some(changes) = downloader.local_changes(&target_dir)? {
            errors.insert(target_key.clone(), changes);
        }

        // VcsCapableDownloaderInterface
        if downloader.vcs_reference(&target_dir)?.is_some() {
            let previous_ref = match package.installation_source() {
                Some(InstallationSource::Source) => package.source_reference(),
                Some(InstallationSource::Dist) => package.dist_reference(),
                _ => None,
            };
            let pkg_config = build_package_config(package);
            let current_version = guesser.guess_version(&pkg_config, &target_dir);
            if let (Some(previous_ref), Some(current_version)) = (previous_ref, current_version) {
                let cur_commit = current_version.commit.as_deref().unwrap_or("");
                let cur_pretty = current_version.pretty_version.as_deref().unwrap_or("");
                if cur_commit != previous_ref && cur_pretty != previous_ref {
                    vcs_version_changes.insert(
                        target_key.clone(),
                        VcsVerChange {
                            previous: VerRef {
                                version: package.pretty_version().to_string(),
                                reference: previous_ref.to_string(),
                            },
                            current: VerRef {
                                version: cur_pretty.to_string(),
                                reference: cur_commit.to_string(),
                            },
                        },
                    );
                }
            }
        }

        // DvcsDownloaderInterface
        if let Some(unpushed) = downloader.unpushed_changes(&target_dir)? {
            unpushed_changes.insert(target_key.clone(), unpushed);
        }
    }

    if errors.is_empty() && unpushed_changes.is_empty() && vcs_version_changes.is_empty() {
        console_writeln_error!(console, "<info>No local changes</info>");
        return Ok(());
    }

    let verbose = cli.verbose > 0;
    let very_verbose = cli.verbose >= 2;

    if !errors.is_empty() {
        console_writeln_error!(
            console,
            "<error>You have changes in the following dependencies:</error>"
        );

        for (path, changes) in &errors {
            if verbose {
                console_writeln!(console, "<info>{path}</info>:");
                console_writeln!(console, "{}", &indent_block(changes));
            } else {
                console_writeln!(console, "{}", path);
            }
        }
    }

    if !unpushed_changes.is_empty() {
        console_writeln_error!(
            console,
            "<warning>You have unpushed changes on the current branch in the following dependencies:</warning>"
        );

        for (path, changes) in &unpushed_changes {
            if verbose {
                console_writeln!(console, "<info>{path}</info>:");
                console_writeln!(console, "{}", &indent_block(changes));
            } else {
                console_writeln!(console, "{}", path);
            }
        }
    }

    if !vcs_version_changes.is_empty() {
        console_writeln_error!(
            console,
            "<warning>You have version variations in the following dependencies:</warning>"
        );

        for (path, change) in &vcs_version_changes {
            if verbose {
                let mut prev = if change.previous.version.is_empty() {
                    change.previous.reference.clone()
                } else {
                    change.previous.version.clone()
                };
                let mut curr = if change.current.version.is_empty() {
                    change.current.reference.clone()
                } else {
                    change.current.version.clone()
                };
                if very_verbose {
                    prev.push_str(&format!(" ({})", change.previous.reference));
                    curr.push_str(&format!(" ({})", change.current.reference));
                }
                console_writeln!(console, "<info>{path}</info>:");
                console_writeln!(
                    console,
                    "    From <comment>{prev}</comment> to <comment>{curr}</comment>"
                );
            } else {
                console_writeln!(console, "{}", path);
            }
        }
    }

    if !verbose {
        console_writeln_error!(console, "Use --verbose (-v) to see a list of files");
    }

    let code = (if !errors.is_empty() { 1 } else { 0 })
        + (if !unpushed_changes.is_empty() { 2 } else { 0 })
        + (if !vcs_version_changes.is_empty() {
            4
        } else {
            0
        });
    if code != 0 {
        return Err(exit_code::bail_silent(code));
    }
    Ok(())
}

fn indent_block(s: &str) -> String {
    s.split('\n')
        .map(|line| format!("    {}", line.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the `package_config` shape that `VersionGuesser` reads. The PHP
/// equivalent is `ArrayDumper::dump($package)`; we only need the fields
/// that `VersionGuesser` actually inspects.
fn build_package_config(package: &LocalPackage) -> serde_json::Value {
    serde_json::json!({
        "extra": package.extra(),
    })
}
