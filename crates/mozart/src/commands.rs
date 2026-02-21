pub mod about;
pub mod archive;
pub mod audit;
pub mod browse;
pub mod bump;
pub mod check_platform_reqs;
pub mod clear_cache;
pub mod config;
pub mod create_project;
pub mod dependency;
pub mod depends;
pub mod diagnose;
pub mod dump_autoload;
pub mod exec;
pub mod fund;
pub mod global;
pub mod init;
pub mod install;
pub mod licenses;
pub mod outdated;
pub mod prohibits;
pub mod reinstall;
pub mod remove;
pub mod repository;
pub mod require;
pub mod run_script;
pub mod search;
pub mod self_update;
pub mod show;
pub mod status;
pub mod suggests;
pub mod update;
pub mod validate;

#[derive(clap::Parser)]
#[command(name = "mozart", version, about = "A PHP dependency manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Increase the verbosity of messages: 1 for normal, 2 for more verbose, 3 for debug
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Display timing and memory usage information
    #[arg(long, global = true)]
    pub profile: bool,

    /// Disables all plugins
    #[arg(long, global = true)]
    pub no_plugins: bool,

    /// Skips execution of all scripts defined in composer.json
    #[arg(long, global = true)]
    pub no_scripts: bool,

    /// If specified, use the given directory as working directory
    #[arg(short = 'd', long = "working-dir", global = true)]
    pub working_dir: Option<String>,

    /// Prevent use of the cache
    #[arg(long, global = true)]
    pub no_cache: bool,

    /// Do not ask any interactive question
    #[arg(short = 'n', long, global = true)]
    pub no_interaction: bool,

    /// Do not output any message
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Force ANSI output
    #[arg(long, global = true)]
    pub ansi: bool,

    /// Disable ANSI output
    #[arg(long, global = true)]
    pub no_ansi: bool,
}

#[derive(clap::Subcommand)]
pub enum Commands {
    /// Short information about Composer
    About(about::AboutArgs),

    /// Creates an archive of this composer package
    Archive(archive::ArchiveArgs),

    /// Checks for security vulnerability advisories for installed packages
    Audit(audit::AuditArgs),

    /// Opens the package's repository URL or homepage in your browser
    #[command(alias = "home")]
    Browse(browse::BrowseArgs),

    /// Increases the lower limit of your package version constraints
    Bump(bump::BumpArgs),

    /// Check that platform requirements are satisfied
    #[command(name = "check-platform-reqs")]
    CheckPlatformReqs(check_platform_reqs::CheckPlatformReqsArgs),

    /// Clears Composer's internal package cache
    #[command(name = "clear-cache", alias = "clearcache", alias = "cc")]
    ClearCache(clear_cache::ClearCacheArgs),

    /// Sets config options
    Config(config::ConfigArgs),

    /// Creates new project from a package into given directory
    #[command(name = "create-project")]
    CreateProject(create_project::CreateProjectArgs),

    /// Shows which packages cause the given package to be installed
    #[command(alias = "why")]
    Depends(depends::DependsArgs),

    /// Diagnoses the system to identify common errors
    Diagnose(diagnose::DiagnoseArgs),

    /// Dumps the autoloader
    #[command(name = "dump-autoload", alias = "dumpautoload")]
    DumpAutoload(dump_autoload::DumpAutoloadArgs),

    /// Executes a vendored binary/script
    Exec(exec::ExecArgs),

    /// Discover how to help fund the maintenance of your dependencies
    Fund(fund::FundArgs),

    /// Allows running commands in the global Composer dir
    Global(global::GlobalArgs),

    /// Creates a basic composer.json file in current directory
    Init(init::InitArgs),

    /// Installs the project dependencies from the composer.lock file if present, or falls back on the composer.json
    #[command(alias = "i")]
    Install(install::InstallArgs),

    /// Shows information about licenses of dependencies
    Licenses(licenses::LicensesArgs),

    /// Shows a list of installed packages that have updates available
    Outdated(outdated::OutdatedArgs),

    /// Shows which packages prevent the given package from being installed
    #[command(alias = "why-not")]
    Prohibits(prohibits::ProhibitsArgs),

    /// Uninstalls and reinstalls the given package names
    Reinstall(reinstall::ReinstallArgs),

    /// Removes a package from the require or require-dev
    #[command(alias = "rm", alias = "uninstall")]
    Remove(remove::RemoveArgs),

    /// Manage repositories
    #[command(alias = "repo")]
    Repository(repository::RepositoryArgs),

    /// Adds required packages to your composer.json and installs them
    #[command(alias = "r")]
    Require(require::RequireArgs),

    /// Runs the scripts defined in composer.json
    #[command(name = "run-script", alias = "run")]
    RunScript(run_script::RunScriptArgs),

    /// Searches for packages
    Search(search::SearchArgs),

    /// Updates Composer to the latest version
    #[command(name = "self-update", alias = "selfupdate")]
    SelfUpdate(self_update::SelfUpdateArgs),

    /// Shows information about packages
    #[command(alias = "info")]
    Show(show::ShowArgs),

    /// Shows a list of locally modified packages
    Status(status::StatusArgs),

    /// Shows package suggestions
    Suggests(suggests::SuggestsArgs),

    /// Updates your dependencies to the latest version according to composer.json
    #[command(alias = "u", alias = "upgrade")]
    Update(update::UpdateArgs),

    /// Validates a composer.json and composer.lock
    Validate(validate::ValidateArgs),
}

pub fn execute(cli: &Cli) -> anyhow::Result<()> {
    let console = crate::console::Console::from_cli(cli);
    match &cli.command {
        Commands::About(args) => about::execute(args, cli, &console),
        Commands::Archive(args) => archive::execute(args, cli, &console),
        Commands::Audit(args) => audit::execute(args, cli, &console),
        Commands::Browse(args) => browse::execute(args, cli, &console),
        Commands::Bump(args) => bump::execute(args, cli, &console),
        Commands::CheckPlatformReqs(args) => check_platform_reqs::execute(args, cli, &console),
        Commands::ClearCache(args) => clear_cache::execute(args, cli, &console),
        Commands::Config(args) => config::execute(args, cli, &console),
        Commands::CreateProject(args) => create_project::execute(args, cli, &console),
        Commands::Depends(args) => depends::execute(args, cli, &console),
        Commands::Diagnose(args) => diagnose::execute(args, cli, &console),
        Commands::DumpAutoload(args) => dump_autoload::execute(args, cli, &console),
        Commands::Exec(args) => exec::execute(args, cli, &console),
        Commands::Fund(args) => fund::execute(args, cli, &console),
        Commands::Global(args) => global::execute(args, cli, &console),
        Commands::Init(args) => init::execute(args, cli, &console),
        Commands::Install(args) => install::execute(args, cli, &console),
        Commands::Licenses(args) => licenses::execute(args, cli, &console),
        Commands::Outdated(args) => outdated::execute(args, cli, &console),
        Commands::Prohibits(args) => prohibits::execute(args, cli, &console),
        Commands::Reinstall(args) => reinstall::execute(args, cli, &console),
        Commands::Remove(args) => remove::execute(args, cli, &console),
        Commands::Repository(args) => repository::execute(args, cli, &console),
        Commands::Require(args) => require::execute(args, cli, &console),
        Commands::RunScript(args) => run_script::execute(args, cli, &console),
        Commands::Search(args) => search::execute(args, cli, &console),
        Commands::SelfUpdate(args) => self_update::execute(args, cli, &console),
        Commands::Show(args) => show::execute(args, cli, &console),
        Commands::Status(args) => status::execute(args, cli, &console),
        Commands::Suggests(args) => suggests::execute(args, cli, &console),
        Commands::Update(args) => update::execute(args, cli, &console),
        Commands::Validate(args) => validate::execute(args, cli, &console),
    }
}
