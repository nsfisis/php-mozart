use crate::composer::Composer;
use clap::Args;
use mozart_core::composer::InstallationSource;
use mozart_core::console::IoInterface;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::exit_code;
use mozart_core::package::dumper::ArrayDumper;
use mozart_core::package::version::{VersionGuesser, VersionParser};

#[derive(Args)]
pub struct StatusArgs {}

pub async fn execute(
    _args: &StatusArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    // init repos
    let composer = Composer::require(cli.working_dir()?)?;

    let installed_repo = composer.repository_manager().local_repository();

    let dm = composer.download_manager();
    let im = composer.installation_manager();

    let mut errors = Vec::new();
    let mut unpushed_changes = Vec::new();
    let mut vcs_version_changes = Vec::new();

    let parser = VersionParser::new();
    let guesser = VersionGuesser::new(parser);
    let dumper = ArrayDumper::new();

    // list packages
    for package in installed_repo.get_canonical_packages() {
        let Some(downloader) = dm.get_downloader_for_package(package) else {
            continue;
        };
        let Some(target_dir) = im.get_install_path(package) else {
            continue;
        };
        let target_dir_key = target_dir.display().to_string();

        if downloader.is_change_report() {
            if std::fs::symlink_metadata(&target_dir)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                errors.push((
                    target_dir_key.clone(),
                    format!("{target_dir_key} is a symbolic link."),
                ));
            }
            if let Some(changes) = downloader.get_local_changes(&target_dir)? {
                errors.push((target_dir_key.clone(), changes));
            }
        }

        if downloader.is_vcs_capable_downloader()
            && downloader.vcs_reference(&target_dir)?.is_some()
        {
            let previous_ref = match package.installation_source() {
                Some(InstallationSource::Source) => package.source_reference(),
                Some(InstallationSource::Dist) => package.dist_reference(),
                _ => None,
            };

            let current_version = guesser.guess_version(&dumper.dump(package), &target_dir);

            if let (Some(previous_ref), Some(current_version)) = (previous_ref, current_version) {
                let current_commit = current_version.commit.as_deref().unwrap_or("");
                let current_pretty_version =
                    current_version.pretty_version.as_deref().unwrap_or("");
                if current_commit != previous_ref && current_pretty_version != previous_ref {
                    vcs_version_changes.push((
                        target_dir_key.clone(),
                        VcsVerChange {
                            previous: VerRef {
                                version: package.pretty_version().to_string(),
                                reference: previous_ref.to_string(),
                            },
                            current: VerRef {
                                version: current_pretty_version.to_string(),
                                reference: current_commit.to_string(),
                            },
                        },
                    ));
                }
            }
        }

        if downloader.is_dvcs_downloader()
            && let Some(unpushed) = downloader.unpushed_changes(&target_dir)?
        {
            unpushed_changes.push((target_dir_key.clone(), unpushed));
        }
    }

    if errors.is_empty() && unpushed_changes.is_empty() && vcs_version_changes.is_empty() {
        console_writeln_error!(io, "<info>No local changes</info>");
        return Ok(());
    }

    if !errors.is_empty() {
        console_writeln_error!(
            io,
            "<error>You have changes in the following dependencies:</error>"
        );

        for (path, changes) in &errors {
            if cli.is_verbose() {
                console_writeln!(io, "<info>{path}</info>:");
                console_writeln!(io, "{}", &indent_block(changes));
            } else {
                console_writeln!(io, "{}", path);
            }
        }
    }

    if !unpushed_changes.is_empty() {
        console_writeln_error!(
            io,
            "<warning>You have unpushed changes on the current branch in the following dependencies:</warning>"
        );

        for (path, changes) in &unpushed_changes {
            if cli.is_verbose() {
                console_writeln!(io, "<info>{path}</info>:");
                console_writeln!(io, "{}", &indent_block(changes));
            } else {
                console_writeln!(io, "{}", path);
            }
        }
    }

    if !vcs_version_changes.is_empty() {
        console_writeln_error!(
            io,
            "<warning>You have version variations in the following dependencies:</warning>"
        );

        for (path, changes) in &vcs_version_changes {
            if cli.is_verbose() {
                // If we don't can't find a version, use the ref instead.
                let mut current_version = if changes.current.version.is_empty() {
                    changes.current.reference.clone()
                } else {
                    changes.current.version.clone()
                };
                let mut previous_version = if changes.previous.version.is_empty() {
                    changes.previous.reference.clone()
                } else {
                    changes.previous.version.clone()
                };

                if io.lock().unwrap().is_very_verbose() {
                    // Output the ref regardless of whether or not it's being used as the version
                    current_version.push_str(&format!(" ({})", changes.current.reference));
                    previous_version.push_str(&format!(" ({})", changes.previous.reference));
                }

                console_writeln!(io, "<info>{path}</info>:");
                console_writeln!(
                    io,
                    "    From <comment>{previous_version}</comment> to <comment>{current_version}</comment>"
                );
            } else {
                console_writeln!(io, "{}", path);
            }
        }
    }

    if !cli.is_verbose() {
        console_writeln_error!(io, "Use --verbose (-v) to see a list of files");
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

struct VcsVerChange {
    previous: VerRef,
    current: VerRef,
}

struct VerRef {
    version: String,
    reference: String,
}

fn indent_block(s: &str) -> String {
    s.split('\n')
        .map(|line| format!("    {}", line.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}
