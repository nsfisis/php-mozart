use crate::composer::Composer;
use clap::Args;
use mozart_core::composer::{InstallationSource, LocalPackage};
use mozart_core::console::Console;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::exit_code;
use mozart_vcs::version_guesser::{VersionGuesser, VersionParser};

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

    let mut errors = Vec::new();
    let mut unpushed_changes = Vec::new();
    let mut vcs_version_changes = Vec::new();

    let parser = VersionParser::new();
    let guesser = VersionGuesser::new(parser);
    let dumper = ArrayDumper::new();

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
        console_writeln_error!(console, "<info>No local changes</info>");
        return Ok(());
    }

    if !errors.is_empty() {
        console_writeln_error!(
            console,
            "<error>You have changes in the following dependencies:</error>"
        );

        for (path, changes) in &errors {
            if cli.is_verbose() {
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
            if cli.is_verbose() {
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
            if cli.is_verbose() {
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
                if console.is_very_verbose() {
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

    if !cli.is_verbose() {
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

/// Mirrors `Composer\Package\Dumper\ArrayDumper`. Serialises a `LocalPackage`
/// into the JSON shape that `VersionGuesser::guess_version` expects.
struct ArrayDumper;

impl ArrayDumper {
    fn new() -> Self {
        Self
    }

    fn dump(&self, package: &LocalPackage) -> serde_json::Value {
        build_package_config(package)
    }
}

/// Serialises a `LocalPackage` to the JSON shape consumed by
/// `VersionGuesser::guess_version`. Mirrors `ArrayDumper::dump($package)` —
/// we include all fields that `VersionGuesser` inspects.
fn build_package_config(package: &LocalPackage) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), package.pretty_name().into());
    obj.insert("version".into(), package.pretty_version().into());
    if let Some(t) = package.package_type() {
        obj.insert("type".into(), t.into());
    }
    obj.insert("extra".into(), package.extra().clone());
    if let Some(src) = package.source() {
        let mut s = serde_json::Map::new();
        s.insert("type".into(), src.kind.clone().into());
        s.insert("url".into(), src.url.clone().into());
        if let Some(r) = &src.reference {
            s.insert("reference".into(), r.clone().into());
        }
        obj.insert("source".into(), serde_json::Value::Object(s));
    }
    if let Some(dist) = package.dist() {
        let mut d = serde_json::Map::new();
        d.insert("type".into(), dist.kind.clone().into());
        d.insert("url".into(), dist.url.clone().into());
        if let Some(r) = &dist.reference {
            d.insert("reference".into(), r.clone().into());
        }
        obj.insert("dist".into(), serde_json::Value::Object(d));
    }
    if let Some(is) = package.installation_source() {
        let s = match is {
            InstallationSource::Source => "source",
            InstallationSource::Dist => "dist",
        };
        obj.insert("installation-source".into(), s.into());
    }
    serde_json::Value::Object(obj)
}
