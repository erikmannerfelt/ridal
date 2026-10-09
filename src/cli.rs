use crate::{formats, gpr, tools, user_metadata};
use clap::{ArgGroup, Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Process one or more GPR profiles into one final output
    Process(ProcessArgs),
    /// Batch-process one or more GPR profiles into many outputs
    BatchProcess(BatchProcessArgs),
    /// Show metadata/location information for one or more GPR profiles
    Info(InfoArgs),
    /// Inspect available processing steps
    Steps(StepsArgs),
    /// Inspect supported formats
    Formats(FormatsArgs),
    /// Render a processed radargram to an image
    Render(RenderArgs),
    /// Work with interpretations (picked layers) of processed radargrams
    Interp(InterpArgs),
    /// Create and inspect Ridal projects
    Project(ProjectArgs),
    /// Create and manage a Ridal site: one server, many projects (#214)
    Site(SiteArgs),
    /// Open a local browser GUI for one radargram or a directory of them
    #[cfg(feature = "server")]
    Gui(GuiArgs),
    /// Run the web server explicitly (for remote or persistent deployment)
    #[cfg(feature = "server")]
    Server(ServerArgs),
}

#[derive(Debug, clap::Args)]
pub struct RenderArgs {
    /// Processed .nc file to render.
    pub input: PathBuf,

    /// Output image path. The extension picks the encoding (.png or .jpg);
    /// if omitted, a sidecar beside the input is used.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Render profile: a built-in name, or a path to a TOML file.
    #[arg(long, default_value = "default")]
    pub profile: String,

    /// Output width in pixels. Defaults to one pixel per trace; larger
    /// than the trace count is not upsampled to.
    #[arg(long)]
    pub width: Option<usize>,

    /// JPEG quality, 1-100. Ignored for PNG.
    #[arg(long)]
    pub quality: Option<u8>,

    /// Render the topographically corrected view instead of the standard
    /// one (#168). Fails with a clear reason rather than falling back to a
    /// standard render when the file lacks usable `elevation`/`depth`
    /// axes.
    #[arg(long)]
    pub topo: bool,

    /// Suppress progress messages.
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Debug, clap::Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a project so interpretations have somewhere to live
    Init(ProjectInitArgs),
    /// Show what a project contains
    Info(ProjectInfoArgs),
    /// Move a project created by an older Ridal into its data directory
    Migrate(ProjectMigrateArgs),
}

#[derive(Debug, clap::Args)]
pub struct ProjectInitArgs {
    /// Directory to create the project in. Created if it does not exist.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Human-facing project name. Cosmetic.
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ProjectInfoArgs {
    /// A path inside the project. The project is found by searching upwards.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct ProjectMigrateArgs {
    /// The project directory, the one holding ridal.toml.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Print what would move, without moving anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, clap::Args)]
pub struct SiteArgs {
    #[command(subcommand)]
    pub command: SiteCommand,
}

/// A site is identity and hosting: server-wide accounts, one session key,
/// and the projects themselves under `projects/`. A project stays portable;
/// inside a site it is addressed by an immutable key.
#[derive(Debug, Subcommand)]
pub enum SiteCommand {
    /// Create a site (a `ridal-site.toml` marker and a `projects/` directory)
    Init(SiteInitArgs),
    /// Manage server-wide accounts
    Account(SiteAccountArgs),
    /// Manage the site's projects
    Project(SiteProjectArgs),
    /// Manage API tokens, which scripts use instead of a password
    Token(SiteTokenArgs),
}

#[derive(Debug, clap::Args)]
pub struct SiteTokenArgs {
    #[command(subcommand)]
    pub command: SiteTokenCommand,
}

#[derive(Debug, Subcommand)]
pub enum SiteTokenCommand {
    /// Create a token for an account and print it. It is shown only once.
    Add(SiteTokenAddArgs),
    /// List tokens, without their secrets
    List(SiteTokenListArgs),
    /// Revoke a token by its id
    Revoke(SiteTokenRevokeArgs),
}

#[derive(Debug, clap::Args)]
pub struct SiteTokenAddArgs {
    /// The account the token acts as.
    pub account: String,

    /// A label for the token, such as `laptop` or `ci`.
    #[arg(long)]
    pub name: String,

    /// A project the token may act in, as PROJECT:ROLE or
    /// PROJECT:ROLE:DOWNLOAD, such as `glac:operator` or
    /// `ice:viewer:results`. Repeat it for more projects. The role and
    /// download scope are ceilings: the token never has more than the
    /// account's membership. Without a download scope, the membership's
    /// applies.
    #[arg(long = "grant", required = true)]
    pub grants: Vec<String>,

    /// How long the token lives: days, weeks or years (`30d`, `12w`, `2y`),
    /// or `never`.
    #[arg(long, default_value = crate::site::tokens::DEFAULT_LIFETIME)]
    pub expires: String,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteTokenListArgs {
    /// Only this account's tokens.
    #[arg(long)]
    pub account: Option<String>,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteTokenRevokeArgs {
    /// The token's id, as `list` shows it.
    pub id: String,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteInitArgs {
    /// Directory to create the site in. Created if it does not exist.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Human-facing site name. Cosmetic.
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct SiteAccountArgs {
    #[command(subcommand)]
    pub command: SiteAccountCommand,
}

#[derive(Debug, Subcommand)]
pub enum SiteAccountCommand {
    /// Create an account and print a one-time invite link
    Add(SiteAccountAddArgs),
    /// Create several accounts, for a class or a workshop, with invite links
    /// or generated passwords
    AddBulk(SiteAccountAddBulkArgs),
    /// List the accounts
    List(SiteAccountListArgs),
    /// Grant or revoke server administration
    Set(SiteAccountSetArgs),
    /// Issue a fresh invite link, for a password reset or a lost one
    Reset(SiteAccountResetArgs),
    /// Remove an account. It is removed from every project.
    Remove(SiteAccountResetArgs),
}

#[derive(Debug, clap::Args)]
pub struct SiteAccountAddArgs {
    /// The account name. Lowercase letters, digits, '-' and '_'.
    pub name: String,

    /// Make this a server administrator: they create projects and accounts,
    /// and act as an administrator in every project.
    #[arg(long)]
    pub server_admin: bool,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteAccountAddBulkArgs {
    /// Number of accounts to create.
    #[arg(long)]
    pub count: usize,

    /// Name them prefix-01, prefix-02, and so on, after any that exist.
    #[arg(long, default_value = "student")]
    pub prefix: String,

    /// Draw names from a fixed pool of friendly usernames instead of the
    /// prefix. Fails if fewer unused names remain than were requested.
    #[arg(long)]
    pub random_names: bool,

    /// Make each account a member of this project (its key). Without it, the
    /// accounts belong to no project until one adds them.
    #[arg(long)]
    pub project: Option<String>,

    /// Their role in --project: viewer, picker, operator or admin.
    #[arg(long, default_value = "picker")]
    pub role: String,

    /// What they may download from --project: none, results, picks, derived
    /// or all.
    #[arg(long, default_value = "all")]
    pub download: String,

    /// Generate shared passwords instead of one-time invite links.
    #[arg(long)]
    pub passwords: bool,

    /// Required with --passwords: generated passwords are shared secrets.
    #[arg(long)]
    pub i_know_what_i_am_doing: bool,

    /// Where --passwords writes `name<TAB>password` lines. They are never
    /// printed to the terminal, which is often captured in a log; hand the
    /// file out and then delete it.
    #[arg(long, default_value = "passwords.txt")]
    pub out: PathBuf,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteAccountListArgs {
    /// A path inside the site. The site is found by searching upwards.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteAccountSetArgs {
    pub name: String,

    /// Grant server administration.
    #[arg(long)]
    pub server_admin: bool,

    /// Revoke server administration.
    #[arg(long, conflicts_with = "server_admin")]
    pub no_server_admin: bool,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteAccountResetArgs {
    pub name: String,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteProjectArgs {
    #[command(subcommand)]
    pub command: SiteProjectCommand,
}

#[derive(Debug, Subcommand)]
pub enum SiteProjectCommand {
    /// Create an empty project at a key
    Add(SiteProjectAddArgs),
    /// List the site's projects
    List(SiteProjectListArgs),
    /// Make a project read-only, keeping its interpretations exportable
    Archive(SiteProjectKeyArgs),
    /// Reverse `archive`
    Unarchive(SiteProjectKeyArgs),
    /// Delete an archived project and everything it owns, for good
    Delete(SiteProjectKeyArgs),
}

#[derive(Debug, clap::Args)]
pub struct SiteProjectAddArgs {
    /// The project's immutable key (lowercase letters, digits, '-' and '_').
    pub key: String,

    /// Human-facing display name. Cosmetic and editable.
    #[arg(long)]
    pub name: Option<String>,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteProjectListArgs {
    /// A path inside the site. The site is found by searching upwards.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct SiteProjectKeyArgs {
    /// The project's key.
    pub key: String,

    /// A path inside the site. The site is found by searching upwards.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct InterpArgs {
    #[command(subcommand)]
    pub command: InterpCommand,
}

#[derive(Debug, Subcommand)]
pub enum InterpCommand {
    /// Derive the level 2 point product from a level 1 interpretation
    Export(InterpExportArgs),
}

#[derive(Debug, clap::Args)]
pub struct InterpExportArgs {
    /// The processed radargram (.nc) the interpretation was drawn on.
    pub radargram: PathBuf,

    /// The level 1 interpretation (a gprinterp JSON document).
    pub interpretation: PathBuf,

    /// Where to write the level 2 product. The format follows the
    /// extension: ".geojson"/".json" for GeoJSON, ".csv" for CSV.
    #[arg(short, long)]
    pub output: PathBuf,

    /// Point spacing along the ground track. A distance in metres ("5",
    /// "2.5"), "auto" to derive one from the radargram's own trace spacing,
    /// "per-trace" for one point per native trace, or "vertices" for the
    /// picked vertices exactly as drawn.
    ///
    /// Spacing is always measured in metres along the track, never in
    /// traces: trace spacing varies with survey speed, so a fixed trace
    /// stride produces unevenly spaced ground positions.
    #[arg(long, default_value = "auto")]
    pub spacing: String,

    /// CRS for the output geometry. WGS84 by default, which is what RFC 7946
    /// requires of GeoJSON. Accepts "native" for the radargram's own
    /// projected CRS, or any CRS string PROJ understands.
    ///
    /// Projected GeoJSON declares its CRS with the 2008 `crs` member, which
    /// GDAL (and so QGIS and GeoPandas) reads, but it is not portable:
    /// readers that follow RFC 7946 strictly will interpret the coordinates
    /// as degrees. Native easting/northing are always present as properties
    /// regardless.
    #[arg(long)]
    pub crs: Option<String>,

    /// The author recorded on every exported point. This command is not
    /// tied to a server account, so it is a label rather than an
    /// authenticated identity.
    #[arg(long, default_value = crate::interp::level2::DEFAULT_USER)]
    pub user: String,
}

#[cfg(feature = "server")]
#[derive(Debug, clap::Args)]
pub struct GuiArgs {
    /// A single processed .nc file, or a directory to scan recursively.
    /// Omitted, Ridal serves the project found by searching upwards from
    /// here, or this directory if there is none.
    pub path: Option<PathBuf>,

    /// In-memory cache budget for encoded chunk/overview images, in MB,
    /// for the whole server: shared by every radargram and project.
    #[arg(long)]
    pub cache_memory_mb: Option<usize>,

    /// Number of worker threads for CPU-heavy rendering.
    #[arg(long)]
    pub n_workers: Option<usize>,

    /// Serve a project without accepting any writes.
    #[arg(long)]
    pub read_only: bool,

    /// Open a browser after starting (off by default, matching
    /// `ridal server start`).
    #[arg(long)]
    pub open_browser: bool,

    /// Port to bind on loopback. Omitted, any free port is used; a fixed
    /// one keeps bookmarks and scripts pointing at the same address.
    #[arg(long)]
    pub port: Option<u16>,
}

#[cfg(feature = "server")]
#[derive(Debug, clap::Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub command: ServerCommand,
}

#[cfg(feature = "server")]
#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    /// Start the HTTP server
    Start(ServerStartArgs),
}

#[cfg(feature = "server")]
#[derive(Debug, clap::Args)]
pub struct ServerStartArgs {
    /// A single processed .nc file, or a directory to scan recursively.
    pub path: PathBuf,

    /// Bind address. Loopback by default; binding elsewhere is explicit
    /// because Ridal does not terminate TLS, so a non-loopback bind needs
    /// a TLS-terminating reverse proxy in front of it (see
    /// `--allow-insecure-login`).
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Bind port. A stable default rather than an OS-assigned ephemeral
    /// port, since this mode is for persistent/remote deployment.
    #[arg(long, default_value_t = 8000)]
    pub port: u16,

    /// Open a browser after starting (off by default in this mode).
    #[arg(long)]
    pub open_browser: bool,

    /// Serve a project without accepting any writes. Caps every caller at
    /// the "viewer" role, whatever their account says.
    #[arg(long)]
    pub read_only: bool,

    /// Accept password logins while bound to a non-loopback address.
    ///
    /// Ridal does not terminate TLS, so a password sent to a non-loopback
    /// address travels in the clear unless something in front of it is
    /// doing so. Use this only when you know what that something is; the
    /// supported arrangement is to bind loopback behind a TLS-terminating
    /// reverse proxy.
    #[arg(long)]
    pub allow_insecure_login: bool,

    /// In-memory cache budget for encoded chunk/overview images, in MB,
    /// for the whole server: shared by every radargram and project.
    #[arg(long)]
    pub cache_memory_mb: Option<usize>,

    /// Number of worker threads for CPU-heavy rendering.
    #[arg(long)]
    pub n_workers: Option<usize>,
}

#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("step_choice")
        .required(false)
        .args(["steps", "default", "default_with_topo"]),
))]
pub struct ProcessArgs {
    /// Input header/data path(s). Explicit paths are preferred, but glob patterns are also expanded.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Velocity of the medium in m/ns. Defaults to the typical velocity of ice.
    #[arg(short, long, default_value = "0.168")]
    pub velocity: f32,

    /// Load a separate ".cor" file (RAMAC only). If not given, it will be searched for automatically.
    #[arg(short, long)]
    pub cor: Option<PathBuf>,

    /// Correct elevation values with a DEM
    #[arg(short, long)]
    pub dem: Option<PathBuf>,

    /// Which coordinate reference system to project coordinates in.
    #[arg(long)]
    pub crs: Option<String>,

    /// Export the location track to CSV. If no value is given, a sidecar path is derived from the output path.
    #[arg(short, long)]
    pub track: Option<Option<PathBuf>>,

    /// Process with the default profile.
    #[arg(long)]
    pub default: bool,

    /// Process with the default profile plus topographic correction.
    #[arg(long = "default-with-topo")]
    pub default_with_topo: bool,

    /// Processing steps to run, separated by commas. Can also be a filepath to a newline-separated step file.
    #[arg(long)]
    pub steps: Option<String>,

    /// Output filename or directory. Defaults to the first input with a ".nc" extension.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Suppress progress messages
    #[arg(short, long)]
    pub quiet: bool,

    /// Render an image of the profile and save it to the specified path. If no path is given, a JPG sidecar is used.
    #[arg(short, long)]
    pub render: Option<Option<PathBuf>>,

    /// Render profile for --render: a built-in name, or a path to a TOML
    /// file. Distinct from the *processing* profile selected by --default
    /// and --steps.
    #[arg(long)]
    pub render_profile: Option<String>,

    /// Output width in pixels for --render. Defaults to one pixel per
    /// trace.
    #[arg(long)]
    pub render_width: Option<usize>,

    /// Render the topographically corrected view for --render, as
    /// `ridal render --topo` does. Fails when the radargram has no usable
    /// elevation and depth axes.
    #[arg(long)]
    pub render_topo: bool,

    /// Don't export an nc file
    #[arg(long)]
    pub no_export: bool,

    /// Override the antenna center frequency (in MHz) from file metadata
    #[arg(long)]
    pub override_antenna_mhz: Option<f32>,

    /// Override the antenna separation (in m) from file metadata
    #[arg(long)]
    pub override_antenna_separation: Option<f32>,

    /// Add user metadata as key=value. Repeatable.
    #[arg(long = "metadata", value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
    pub metadata: Vec<String>,

    /// Stable, unique identifier for this radargram (lowercase ASCII, digits, '-', '_').
    /// Defaults to the output file stem if not given.
    #[arg(long = "radargram-id")]
    pub radargram_id: Option<String>,

    /// Human-readable display label. Purely cosmetic: has no identity semantics.
    #[arg(long = "display-name")]
    pub display_name: Option<String>,

    /// Human-readable name of the group this radargram belongs to (survey,
    /// campaign, location), for catalog grouping. Non-ASCII is accepted and
    /// written unchanged. Ridal writes everything it generates itself as
    /// ASCII, but some NetCDF readers (for example xarray with the h5netcdf
    /// engine) will garble a non-ASCII value. A stable URL/filesystem-safe id
    /// is derived from this automatically unless --group-id overrides it.
    /// `--group` is a supported alias.
    #[arg(long = "group-name", alias = "group")]
    pub group: Option<String>,

    /// Explicit override for the group's id, when the id automatically
    /// derived from --group-name is not the one wanted.
    #[arg(long = "group-id")]
    pub group_id: Option<String>,
}

#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("step_choice")
        .required(false)
        .args(["steps", "default", "default_with_topo"]),
))]
pub struct BatchProcessArgs {
    /// Input header/data path(s). Explicit paths are preferred, but glob patterns are also expanded.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Output directory. Must already exist.
    #[arg(short, long, required = true)]
    pub output: PathBuf,

    /// Velocity of the medium in m/ns. Defaults to the typical velocity of ice.
    #[arg(short, long, default_value = "0.168")]
    pub velocity: f32,

    /// Load a separate ".cor" file (RAMAC only). If not given, it will be searched for automatically.
    #[arg(short, long)]
    pub cor: Option<PathBuf>,

    /// Correct elevation values with a DEM
    #[arg(short, long)]
    pub dem: Option<PathBuf>,

    /// Which coordinate reference system to project coordinates in.
    #[arg(long)]
    pub crs: Option<String>,

    /// Export location tracks to CSV in the given directory.
    #[arg(short, long)]
    pub track: Option<Option<PathBuf>>,

    /// Process with the default profile.
    #[arg(long)]
    pub default: bool,

    /// Process with the default profile plus topographic correction.
    #[arg(long = "default-with-topo")]
    pub default_with_topo: bool,

    /// Processing steps to run, separated by commas. Can also be a filepath to a newline-separated step file.
    #[arg(long)]
    pub steps: Option<String>,

    /// Suppress progress messages
    #[arg(short, long)]
    pub quiet: bool,

    /// Render images into the given directory.
    #[arg(short, long)]
    pub render: Option<Option<PathBuf>>,

    /// Render profile for --render: a built-in name, or a path to a TOML
    /// file. Distinct from the *processing* profile selected by --default
    /// and --steps.
    #[arg(long)]
    pub render_profile: Option<String>,

    /// Output width in pixels for --render. Defaults to one pixel per
    /// trace.
    #[arg(long)]
    pub render_width: Option<usize>,

    /// Render the topographically corrected view for --render, as
    /// `ridal render --topo` does. Fails when the radargram has no usable
    /// elevation and depth axes.
    #[arg(long)]
    pub render_topo: bool,

    /// Don't export nc files
    #[arg(long)]
    pub no_export: bool,

    /// Merge neighboring chronological profiles that are closer than the given threshold
    /// (e.g. "10 min"). Incompatible neighbors remain separate outputs.
    #[arg(long)]
    pub merge: Option<String>,

    /// Override the antenna center frequency (in MHz) from file metadata
    #[arg(long)]
    pub override_antenna_mhz: Option<f32>,

    /// Override the antenna separation (in m) from file metadata
    #[arg(long)]
    pub override_antenna_separation: Option<f32>,

    /// Add user metadata as key=value. Repeatable.
    #[arg(long = "metadata", value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
    pub metadata: Vec<String>,

    /// Human-readable name of the group all outputs in this batch belong to
    /// (survey, campaign, location), for catalog grouping. Non-ASCII is
    /// accepted and written unchanged; see `ridal process --help` for the
    /// reader caveat. Applied uniformly; radargram IDs and display names are
    /// still derived per-output since an explicit single value would collide.
    /// `--group` is a supported alias.
    #[arg(long = "group-name", alias = "group")]
    pub group: Option<String>,

    /// Explicit override for the group's id, when the id automatically
    /// derived from --group-name is not the one wanted.
    #[arg(long = "group-id")]
    pub group_id: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct InfoArgs {
    /// Input header/data path(s), or NetCDF files Ridal processed, which are
    /// reported by their radargram and revision ids. Explicit paths are
    /// preferred, but glob patterns are also expanded.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Emit JSON instead of human-readable text
    #[arg(long)]
    pub json: bool,

    /// Velocity of the medium in m/ns. Defaults to the typical velocity of ice.
    #[arg(short, long, default_value = "0.168")]
    pub velocity: f32,

    /// Load a separate ".cor" file (RAMAC only). If not given, it will be searched for automatically.
    #[arg(short, long)]
    pub cor: Option<PathBuf>,

    /// Correct elevation values with a DEM
    #[arg(short, long)]
    pub dem: Option<PathBuf>,

    /// Which coordinate reference system to project coordinates in.
    #[arg(long)]
    pub crs: Option<String>,

    /// Override the antenna center frequency (in MHz) from file metadata
    #[arg(long)]
    pub override_antenna_mhz: Option<f32>,

    /// Override the antenna separation (in m) from file metadata
    #[arg(long)]
    pub override_antenna_separation: Option<f32>,
}

#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("steps_mode")
        .required(false)
        .args(["describe_all", "describe", "default"]),
))]
pub struct StepsArgs {
    /// Show descriptions for all steps
    #[arg(long = "describe-all")]
    pub describe_all: bool,

    /// Show the description for one step
    #[arg(long)]
    pub describe: Option<String>,

    /// Show the default processing pipeline
    #[arg(long)]
    pub default: bool,

    /// Emit JSON instead of human-readable text
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Args)]
pub struct FormatsArgs {
    /// Emit JSON instead of human-readable text
    #[arg(long)]
    pub json: bool,
}

fn resolve_steps(
    default: bool,
    default_with_topo: bool,
    steps: Option<&str>,
) -> Result<Vec<String>, String> {
    let resolved_steps = if default_with_topo {
        let mut profile = gpr::default_processing_profile();
        profile.push("correct_topography".to_string());
        profile
    } else if default {
        gpr::default_processing_profile()
    } else if let Some(step_text) = steps {
        tools::parse_step_list(step_text)?
    } else {
        vec![]
    };

    gpr::validate_steps(&resolved_steps)?;
    Ok(resolved_steps)
}

fn optional_existing_dir(
    value: &Option<Option<PathBuf>>,
    label: &str,
) -> Result<Option<PathBuf>, String> {
    match value {
        None => Ok(None),
        Some(None) => Ok(None),
        Some(Some(path)) => {
            if !path.is_dir() {
                Err(format!(
                    "{label} must be an existing directory in batch mode: {}",
                    path.display()
                ))
            } else {
                Ok(Some(path.clone()))
            }
        }
    }
}

#[cfg(feature = "cli")]
#[allow(dead_code)]
pub fn main(arguments: Args) -> i32 {
    match run(arguments) {
        Ok(()) => 0,
        Err(message) => error(&message, 1),
    }
}

pub fn run(arguments: Args) -> Result<(), String> {
    match arguments.command {
        Commands::Process(args) => process_command(&args),
        Commands::BatchProcess(args) => batch_process_command(&args),
        Commands::Info(args) => info_command(args),
        Commands::Steps(args) => steps_command(args),
        Commands::Formats(args) => formats_command(args),
        Commands::Render(args) => render_command(args),
        Commands::Interp(args) => match args.command {
            InterpCommand::Export(args) => interp_export_command(&args),
        },
        Commands::Project(args) => match args.command {
            ProjectCommand::Init(args) => project_init_command(&args),
            ProjectCommand::Info(args) => project_info_command(&args),
            ProjectCommand::Migrate(args) => project_migrate_command(&args),
        },
        #[cfg(feature = "server")]
        Commands::Gui(args) => gui_command(args),
        #[cfg(feature = "server")]
        Commands::Server(args) => server_command(args),
        Commands::Site(args) => site_command(args),
    }
}

#[cfg(feature = "server")]
fn render_service_config(
    cache_memory_mb: Option<usize>,
    n_workers: Option<usize>,
) -> Result<crate::server::render_service::RenderServiceConfig, String> {
    // Rejected rather than silently clamped: n_workers sizes the render
    // permit semaphore, and zero permits would leave every image request
    // waiting until it times out into a 503. A user who typed 0 meant
    // something, and it is not that.
    if n_workers == Some(0) {
        return Err("--n-workers must be at least 1".to_string());
    }
    let default = crate::server::render_service::RenderServiceConfig::default();
    let n_workers = n_workers.unwrap_or(default.n_workers);
    Ok(crate::server::render_service::RenderServiceConfig {
        cache: crate::server::render_service::RenderCache::from_mb(
            cache_memory_mb.unwrap_or(crate::server::render_service::DEFAULT_CACHE_MEMORY_MB),
        ),
        ..default
    }
    .with_n_workers(n_workers))
}

#[cfg(feature = "server")]
fn gui_command(args: GuiArgs) -> Result<(), String> {
    let config = render_service_config(args.cache_memory_mb, args.n_workers)?;
    let path = gui_root(args.path.as_deref());
    crate::server::launch::run_gui(
        &path,
        args.read_only,
        args.open_browser,
        args.port.unwrap_or(0),
        config,
    )
}

/// What `ridal gui` serves when it was given no path.
///
/// The project root found by walking upwards, the way `cargo` and `git`
/// work from anywhere inside a tree (#187) -- so `ridal gui` in
/// `my_survey/2024/` serves the whole survey rather than one year of it,
/// which is also where the interpretations already are. With no project
/// above, it is the current directory, which is what it always was.
#[cfg(feature = "server")]
fn gui_root(path: Option<&std::path::Path>) -> PathBuf {
    match path {
        Some(path) => path.to_path_buf(),
        None => {
            let here = PathBuf::from(".");
            match crate::project::Project::find_root(&here) {
                Some(root) => {
                    println!("Serving the project at {}", root.display());
                    root
                }
                None => here,
            }
        }
    }
}

#[cfg(feature = "server")]
fn server_command(args: ServerArgs) -> Result<(), String> {
    match args.command {
        ServerCommand::Start(start_args) => {
            let host: std::net::IpAddr = start_args
                .host
                .parse()
                .map_err(|e| format!("Invalid --host '{}': {e}", start_args.host))?;
            let config = render_service_config(start_args.cache_memory_mb, start_args.n_workers)?;
            crate::server::launch::run_server_start(
                &start_args.path,
                host,
                start_args.port,
                start_args.open_browser,
                start_args.read_only,
                start_args.allow_insecure_login,
                config,
            )
        }
    }
}

fn process_command(args: &ProcessArgs) -> Result<(), String> {
    let resolved_steps =
        resolve_steps(args.default, args.default_with_topo, args.steps.as_deref())?;
    let user_metadata = user_metadata::parse_cli_metadata(&args.metadata)?;

    let params = gpr::RunParams {
        filepaths: args.inputs.clone(),
        output_path: args.output.clone(),
        dem_path: args.dem.clone(),
        cor_path: args.cor.clone(),
        medium_velocity: args.velocity,
        crs: args.crs.clone(),
        quiet: args.quiet,
        track_path: args.track.clone(),
        steps: resolved_steps,
        no_export: args.no_export,
        render_path: args.render.clone(),
        render_profile: args.render_profile.clone(),
        render_width: args.render_width,
        render_topo: args.render_topo,
        override_antenna_mhz: args.override_antenna_mhz,
        override_antenna_separation: args.override_antenna_separation,
        user_metadata,
        radargram_id: args.radargram_id.clone(),
        display_name: args.display_name.clone(),
        group: args.group.clone(),
        group_id: args.group_id.clone(),
    };

    let result = gpr::run(params).map_err(|e| e.to_string())?;
    if !args.quiet {
        println!("{}", result.output_path.display());
    }
    Ok(())
}
fn batch_process_command(args: &BatchProcessArgs) -> Result<(), String> {
    if !args.output.is_dir() {
        return Err(format!(
            "output must be an existing directory in batch mode: {}",
            args.output.display()
        ));
    }

    let render_dir = optional_existing_dir(&args.render, "render")?;
    let track_dir = optional_existing_dir(&args.track, "track")?;

    let resolved_steps =
        resolve_steps(args.default, args.default_with_topo, args.steps.as_deref())?;
    let user_metadata = user_metadata::parse_cli_metadata(&args.metadata)?;

    let params = gpr::BatchRunParams {
        filepaths: args.inputs.clone(),
        output_dir: args.output.clone(),
        dem_path: args.dem.clone(),
        cor_path: args.cor.clone(),
        medium_velocity: args.velocity,
        crs: args.crs.clone(),
        quiet: args.quiet,
        track_dir,
        steps: resolved_steps,
        no_export: args.no_export,
        render_dir,
        render_profile: args.render_profile.clone(),
        render_width: args.render_width,
        render_topo: args.render_topo,
        merge: args.merge.clone(),
        override_antenna_mhz: args.override_antenna_mhz,
        override_antenna_separation: args.override_antenna_separation,
        user_metadata,
        group: args.group.clone(),
        group_id: args.group_id.clone(),
    };

    let result = gpr::run_batch(params)?;
    if !args.quiet {
        for path in result.output_paths {
            println!("{}", path.display());
        }
    }
    Ok(())
}
fn info_command(args: InfoArgs) -> Result<(), String> {
    let params = gpr::InfoParams {
        filepaths: args.inputs,
        dem_path: args.dem,
        cor_path: args.cor,
        medium_velocity: args.velocity,
        crs: args.crs,
        override_antenna_mhz: args.override_antenna_mhz,
        override_antenna_separation: args.override_antenna_separation,
    };
    let records = gpr::inspect(params).map_err(|e| format!("{e:?}"))?;
    if args.json {
        if records.len() == 1 {
            println!(
                "{}",
                serde_json::to_string_pretty(&records[0]).map_err(|e| e.to_string())?
            );
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&records).map_err(|e| e.to_string())?
            );
        }
    } else {
        for (i, record) in records.iter().enumerate() {
            if i > 0 {
                println!();
            }
            match record {
                gpr::Inspected::Raw(record) => print_info_record(record),
                gpr::Inspected::Processed(record) => print_processed_info_record(record),
            }
        }
    }
    Ok(())
}

fn steps_command(args: StepsArgs) -> Result<(), String> {
    let all_steps = gpr::all_available_steps();
    if args.json {
        if args.default {
            println!(
                "{}",
                serde_json::to_string_pretty(&gpr::default_processing_profile())
                    .map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        if let Some(step_name) = args.describe {
            let mapping = step_mapping(Some(step_name), &all_steps)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&mapping).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        if args.describe_all {
            let mapping = step_mapping(None, &all_steps)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&mapping).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        let names = all_steps
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<String>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&names).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    if args.default {
        for step in gpr::default_processing_profile() {
            println!("{step}");
        }
        return Ok(());
    }
    if let Some(step_name) = args.describe {
        let mapping = step_mapping(Some(step_name), &all_steps)?;
        for (name, description) in mapping {
            println!("{name}\n{}\n{description}\n", "-".repeat(name.len()));
        }
        return Ok(());
    }
    if args.describe_all {
        for (name, description) in all_steps {
            println!("{name}\n{}\n{description}\n", "-".repeat(name.len()));
        }
        return Ok(());
    }

    for (name, _) in all_steps {
        println!("{name}");
    }
    Ok(())
}

/// `ridal render <input.nc> [-o out.png] [--profile ...] [--width ...]`
///
/// The command-line half of the web GUI's image download. Both go through
/// `render::oneshot`, so the two produce the same picture from the same
/// file and profile.
fn render_command(args: RenderArgs) -> Result<(), String> {
    let profile = crate::render::profile::RenderProfile::resolve(&args.profile)?;
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| crate::render::oneshot::sidecar_path(&args.input, &profile));

    let request = crate::render::oneshot::RenderRequest {
        profile: &profile,
        width: args.width,
        quality: args.quality,
    };

    let (width, height) = if args.topo {
        crate::render::oneshot::render_topo_path_to_file(&args.input, &output, &request)?
    } else {
        crate::render::oneshot::render_path_to_file(&args.input, &output, &request)?
    };

    if !args.quiet {
        println!(
            "Rendered {}x{} px with the '{}' profile to {:?}",
            width, height, profile.name, output
        );
    }
    Ok(())
}

fn formats_command(args: FormatsArgs) -> Result<(), String> {
    let all_formats = formats::all_formats();
    if args.json {
        let payload = serde_json::json!({ "formats": all_formats });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    for fmt in all_formats {
        println!("{}", fmt.name);
        println!("{}", "-".repeat(fmt.name.len()));
        println!("{}", fmt.description);
        println!("Read:  {}", fmt.capabilities.read);
        println!("Write: {}", fmt.capabilities.write);
        println!(
            "Files:  header={} data={} coordinates={}",
            fmt.files.header, fmt.files.data, fmt.files.coordinates
        );
        println!();
    }
    Ok(())
}

// TODO: Might not be used anywhere (2026-03-28)
#[allow(dead_code)]
fn choose_steps(
    default: bool,
    default_with_topo: bool,
    steps: Option<&str>,
) -> Result<Vec<String>, String> {
    if default_with_topo {
        let mut profile = gpr::default_processing_profile();
        profile.push("correct_topography".to_string());
        return Ok(profile);
    }
    if default {
        return Ok(gpr::default_processing_profile());
    }
    match steps {
        Some(step_text) => tools::parse_step_list(step_text),
        None => Ok(vec![]),
    }
}

fn step_mapping(
    only_name: Option<String>,
    all_steps: &[(String, String)],
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::<String, String>::new();
    for (name, description) in all_steps {
        if let Some(target) = &only_name {
            if name != target {
                continue;
            }
        }
        out.insert(name.clone(), description.clone());
    }
    if let Some(target) = only_name {
        if out.is_empty() {
            return Err(format!("Unknown step: {target}"));
        }
    }
    Ok(out)
}

fn print_processed_info_record(record: &gpr::ProcessedInfoRecord) {
    let or_none = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_string());
    println!("Input:\t\t{}", record.input);
    println!(
        "Format:\t\t{} ({})",
        record.format.name, record.format.description
    );
    println!("Ridal version:\t{}", record.ridal_version);
    if let Some(reason) = &record.reprocess_reason {
        println!("Reprocess:\t{reason}");
        return;
    }
    println!();
    println!("Identity");
    println!("--------");
    println!("Radargram id:\t{}", or_none(&record.radargram_id));
    println!("Display name:\t{}", or_none(&record.display_name));
    println!("Group:\t\t{}", or_none(&record.group_name));
    println!("Group id:\t{}", or_none(&record.group_id));
    println!("Processed:\t{}", or_none(&record.processing_datetime));
    println!("Revision id:\t{}", or_none(&record.revision_id));
    if let (Some(samples), Some(traces)) = (record.samples, record.traces) {
        println!("Shape:\t\t{samples} samples x {traces} traces");
    }
}

fn print_info_record(record: &gpr::InfoRecord) {
    println!("Input:\t\t{}", record.input);
    println!(
        "Format:\t\t{} ({})",
        record.format.name, record.format.description
    );
    println!("Header:\t\t{}", record.related_files.header);
    println!("Data:\t\t{}", record.related_files.data);
    println!("Coordinates:\t{}", record.related_files.coordinates);
    println!();
    println!("Metadata");
    println!("--------");
    println!("Samples:\t{}", record.metadata.samples);
    println!("Traces:\t\t{}", record.metadata.last_trace);
    println!("Time window:\t{} ns", record.metadata.time_window_ns);
    println!(
        "Velocity:\t{} m/ns",
        record.metadata.medium_velocity_m_per_ns
    );
    println!(
        "Sampling freq:\t{} MHz",
        record.metadata.sampling_frequency_mhz
    );
    println!("Antenna:\t{}", record.metadata.antenna_name);
    println!("Antenna MHz:\t{}", record.metadata.antenna_mhz);
    println!("Antenna sep:\t{} m", record.metadata.antenna_separation_m);
    println!();
    println!("Location");
    println!("--------");
    println!("Points:\t\t{}", record.location.n_points);
    println!("CRS:\t\t{}", record.location.crs);
    println!("Start:\t\t{}", record.location.start_time);
    println!("Stop:\t\t{}", record.location.stop_time);
    println!("Duration:\t{:.3} s", record.location.duration_s);
    println!("Track length:\t{:.3} m", record.location.track_length_m);
    println!(
        "Altitude:\t{:.3} - {:.3} m",
        record.location.altitude_min_m, record.location.altitude_max_m
    );
    println!(
        "Centroid:\tE {:.3}, N {:.3}, Z {:.3}",
        record.location.centroid.easting,
        record.location.centroid.northing,
        record.location.centroid.altitude
    );
    println!(
        "Correction:\t{}{}",
        record.location.correction.kind,
        record
            .location
            .correction
            .source
            .as_ref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default()
    );
}

#[cfg(feature = "cli")]
#[allow(dead_code)]
fn error(message: &str, code: i32) -> i32 {
    eprintln!("{message}");
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[cfg(feature = "server")]
    fn project_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        crate::project::Project::init(dir.path(), Some("test")).unwrap();
        dir
    }

    #[cfg(feature = "server")]
    #[test]
    fn gui_without_a_path_serves_the_project_found_above_it() {
        // The cargo/git behaviour: run it from anywhere inside the tree and
        // it works on the whole tree, which for Ridal is also where the
        // interpretations already are (#187).
        let dir = project_dir();
        let nested = dir.path().join("2024").join("day-3");
        std::fs::create_dir_all(&nested).unwrap();

        let found = crate::project::Project::find_root(&nested).unwrap();
        assert_eq!(
            found,
            std::fs::canonicalize(dir.path()).unwrap(),
            "a subdirectory must not be served as if it were the project"
        );

        // An explicit path is still exactly what it says, project or not.
        assert_eq!(super::gui_root(Some(&nested)), nested);

        // And outside any project, the answer is the directory itself --
        // the read-only arrangement Ridal has always had.
        let bare = tempfile::tempdir().unwrap();
        assert!(crate::project::Project::find_root(bare.path()).is_none());
    }

    #[test]
    fn migrating_is_reported_and_dry_running_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("interpretations/line-01")).unwrap();
        std::fs::write(
            dir.path()
                .join("interpretations/line-01/erik.gprinterp.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(dir.path().join(crate::project::MARKER), "[project]\n").unwrap();

        super::project_migrate_command(&ProjectMigrateArgs {
            path: dir.path().to_path_buf(),
            dry_run: true,
        })
        .unwrap();
        assert!(
            dir.path().join("interpretations").exists(),
            "dry run moved it"
        );

        super::project_migrate_command(&ProjectMigrateArgs {
            path: dir.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();
        assert!(!dir.path().join("interpretations").exists());
        crate::project::Project::open(dir.path()).unwrap();

        // Running it again is a no-op rather than an error: the answer to
        // "did that work?" should not depend on how many times it was run.
        super::project_migrate_command(&ProjectMigrateArgs {
            path: dir.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();
    }

    /// A site with a server administrator and one project, `glac`.
    fn site_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let site = crate::site::Site::init(dir.path(), Some("test")).unwrap();
        crate::site::accounts::update(site.store(), |set| {
            set.users.push(crate::site::accounts::Account::new(
                crate::identity::UserId::new("anna").unwrap(),
                true,
            ));
            Ok(())
        })
        .unwrap();
        site.create_project(
            &crate::identity::ProjectKey::new("glac").unwrap(),
            None,
            None,
        )
        .unwrap();
        dir
    }

    fn add_bulk(dir: &std::path::Path, extra: &[&str]) -> Result<(), String> {
        let mut argv = vec![
            "ridal",
            "site",
            "account",
            "add-bulk",
            "--path",
            dir.to_str().unwrap(),
        ];
        argv.extend_from_slice(extra);
        match Args::try_parse_from(argv).unwrap().command {
            Commands::Site(SiteArgs {
                command:
                    SiteCommand::Account(SiteAccountArgs {
                        command: SiteAccountCommand::AddBulk(args),
                    }),
            }) => super::site_account_add_bulk_command(&args),
            other => panic!("expected site account add-bulk, got {other:?}"),
        }
    }

    fn site_accounts(dir: &std::path::Path) -> crate::site::accounts::AccountSet {
        let site = crate::site::Site::open(dir).unwrap();
        crate::site::accounts::read(site.store())
            .unwrap()
            .unwrap()
            .0
    }

    /// Run `ridal site token …` against the site at `dir`.
    fn site_token(dir: &std::path::Path, argv: &[&str]) -> Result<(), String> {
        let mut full = vec!["ridal", "site", "token"];
        full.extend_from_slice(argv);
        // `list` takes the site as a positional path, the others as --path.
        if argv.first() == Some(&"list") {
            full.push(dir.to_str().unwrap());
        } else {
            full.extend_from_slice(&["--path", dir.to_str().unwrap()]);
        }
        super::run(Args::try_parse_from(full).unwrap())
    }

    fn site_tokens(dir: &std::path::Path) -> crate::site::tokens::TokenSet {
        let site = crate::site::Site::open(dir).unwrap();
        crate::site::tokens::read(site.store()).unwrap().0
    }

    #[test]
    fn tokens_are_added_listed_and_revoked_from_the_command_line() {
        use crate::project::roles::{DownloadScope, Role};
        let dir = site_dir();
        site_token(
            dir.path(),
            &[
                "add",
                "anna",
                "--name",
                "ci",
                "--grant",
                "glac:operator",
                "--expires",
                "30d",
            ],
        )
        .unwrap();
        let set = site_tokens(dir.path());
        assert_eq!(set.tokens.len(), 1);
        let token = &set.tokens[0];
        assert_eq!(token.name, "ci");
        assert_eq!(token.grants[0].role, Role::Operator);
        // No download scope given: a server administrator's is everything.
        assert_eq!(token.grants[0].download, DownloadScope::All);
        assert_eq!(token.expires, Some(token.created + 30 * 86_400));

        site_token(dir.path(), &["list"]).unwrap();
        site_token(dir.path(), &["list", "--account", "anna"]).unwrap();
        let id = token.id.clone();
        site_token(dir.path(), &["revoke", &id]).unwrap();
        assert!(site_tokens(dir.path()).tokens.is_empty());
        site_token(dir.path(), &["list"]).unwrap();
        assert!(
            site_token(dir.path(), &["revoke", &id]).is_err(),
            "gone already"
        );

        // Both are in the site history, from the command line.
        let site = crate::site::Site::open(dir.path()).unwrap();
        let history = std::fs::read_to_string(site.store().root().join("audit.jsonl")).unwrap();
        assert!(history.contains("token_created") && history.contains("token_revoked"));
        assert!(history.contains("\"cli\""), "{history}");
    }

    #[test]
    fn a_command_line_grant_is_checked_like_any_other() {
        use crate::project::roles::{DownloadScope, Role};
        let dir = site_dir();
        // bob is a viewer in glac who may download results only.
        let site = crate::site::Site::open(dir.path()).unwrap();
        let bob = crate::identity::UserId::new("bob").unwrap();
        crate::site::accounts::update(site.store(), |set| {
            set.users
                .push(crate::site::accounts::Account::new(bob.clone(), false));
            Ok(())
        })
        .unwrap();
        let project = site
            .project(&crate::identity::ProjectKey::new("glac").unwrap())
            .unwrap();
        crate::project::members::update(project.documents(), |set| {
            set.upsert(&bob, Role::Viewer, DownloadScope::Results);
            Ok(())
        })
        .unwrap();

        let add = |extra: &[&str]| {
            let mut argv = vec!["add", "bob", "--name", "x"];
            argv.extend_from_slice(extra);
            site_token(dir.path(), &argv)
        };
        for (grant, why) in [
            ("glac", "no role"),
            ("glac:viewer:results:more", "too many parts"),
            ("glac:boss", "no such role"),
            ("glac:viewer:everything", "no such scope"),
            ("Glac:viewer", "not a key"),
            ("glac:operator", "above the role"),
            ("glac:viewer:all", "above the download scope"),
            ("elsewhere:viewer", "not a member"),
        ] {
            assert!(add(&["--grant", grant]).is_err(), "{why}");
        }
        assert!(add(&["--grant", "glac:viewer", "--expires", "soon"]).is_err());
        assert!(site_token(
            dir.path(),
            &["add", "nobody", "--name", "x", "--grant", "glac:viewer"]
        )
        .is_err());
        assert!(
            site_tokens(dir.path()).tokens.is_empty(),
            "nothing was half-made"
        );

        // Without a scope, the membership's: not 'all', which would be refused.
        add(&["--grant", "glac:viewer"]).unwrap();
        let set = site_tokens(dir.path());
        assert_eq!(set.tokens[0].grants[0].download, DownloadScope::Results);
        assert_eq!(
            set.tokens[0].expires,
            Some(set.tokens[0].created + 90 * 86_400)
        );
    }

    #[test]
    fn bulk_invites_from_the_command_line_carry_the_project() {
        let dir = site_dir();
        add_bulk(
            dir.path(),
            &["--count", "3", "--project", "glac", "--role", "viewer"],
        )
        .unwrap();
        // Continuing a batch numbers after it rather than colliding.
        add_bulk(dir.path(), &["--count", "1"]).unwrap();

        let set = site_accounts(dir.path());
        let names: Vec<&str> = set.users.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "anna",
                "student-01",
                "student-02",
                "student-03",
                "student-04"
            ]
        );
        let invite = set.users[1].invite.as_ref().expect("an invite");
        assert_eq!(invite.project.as_ref().unwrap().as_str(), "glac");
        assert_eq!(invite.role, Some(crate::project::roles::Role::Viewer));
        assert!(set.users[4].invite.as_ref().unwrap().project.is_none());
        assert!(set.users.iter().skip(1).all(|a| a.password_hash.is_none()));
    }

    #[cfg(feature = "server")]
    #[test]
    fn bulk_passwords_need_the_flag_refuse_admins_and_write_a_handout() {
        let dir = site_dir();
        let out = dir.path().join("handout.txt");
        let out = out.to_str().unwrap();
        let passwords = ["--count", "2", "--passwords", "--project", "glac"];

        let refused = add_bulk(dir.path(), &passwords).unwrap_err();
        assert!(refused.contains("--i-know-what-i-am-doing"), "{refused}");

        let mut admins = passwords.to_vec();
        admins.extend(["--i-know-what-i-am-doing", "--role", "admin"]);
        let refused = add_bulk(dir.path(), &admins).unwrap_err();
        assert!(refused.contains("invite links"), "{refused}");
        assert_eq!(site_accounts(dir.path()).users.len(), 1, "nothing created");

        let mut ok = passwords.to_vec();
        ok.extend(["--i-know-what-i-am-doing", "--out", out]);
        add_bulk(dir.path(), &ok).unwrap();

        let handout = std::fs::read_to_string(out).unwrap();
        let lines: Vec<&str> = handout.lines().collect();
        assert_eq!(lines.len(), 2);
        let (name, password) = lines[0].split_once('\t').unwrap();
        let set = site_accounts(dir.path());
        let account = set
            .get(&crate::identity::UserId::new(name).unwrap())
            .unwrap();
        assert!(crate::site::accounts::verify_password(account, password));

        // No invite to redeem, so the membership is there already.
        let site = crate::site::Site::open(dir.path()).unwrap();
        let project = site
            .project(&crate::identity::ProjectKey::new("glac").unwrap())
            .unwrap();
        let (members, _) = crate::project::members::read(project.documents())
            .unwrap()
            .unwrap();
        assert_eq!(members.members.len(), 2);
    }

    /// Run one `ridal site …` command line.
    fn site(argv: &[&str]) -> Result<(), String> {
        let mut full = vec!["ridal", "site"];
        full.extend_from_slice(argv);
        match Args::try_parse_from(full).unwrap().command {
            Commands::Site(args) => super::site_command(args),
            other => panic!("expected a site command, got {other:?}"),
        }
    }

    #[test]
    fn a_site_is_managed_from_the_command_line_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let name = |name: &str| crate::identity::UserId::new(name).unwrap();
        let key = |key: &str| crate::identity::ProjectKey::new(key).unwrap();

        site(&["init", root, "--name", "Course"]).unwrap();
        assert!(site(&["init", root]).is_err(), "a site is made once");

        // The first account has to be able to administer the site.
        let refused = site(&["account", "add", "bo", "--path", root]).unwrap_err();
        assert!(refused.contains("--server-admin"), "{refused}");
        site(&["account", "add", "anna", "--server-admin", "--path", root]).unwrap();
        site(&["account", "add", "bo", "--path", root]).unwrap();
        assert!(site(&["account", "add", "bo", "--path", root]).is_err());
        site(&["account", "list", root]).unwrap();

        // Server administration: granted, and never taken from the last one.
        assert!(site(&["account", "set", "bo", "--path", root]).is_err());
        site(&["account", "set", "bo", "--server-admin", "--path", root]).unwrap();
        site(&["account", "set", "bo", "--no-server-admin", "--path", root]).unwrap();
        let refused = site(&[
            "account",
            "set",
            "anna",
            "--no-server-admin",
            "--path",
            root,
        ])
        .unwrap_err();
        assert!(refused.contains("only server administrator"), "{refused}");
        assert!(site(&["account", "set", "cy", "--server-admin", "--path", root]).is_err());
        site(&["account", "reset", "bo", "--path", root]).unwrap();
        assert!(site(&["account", "reset", "cy", "--path", root]).is_err());

        // Projects, and the two steps to remove one.
        site(&["project", "list", root]).unwrap();
        site(&[
            "project",
            "add",
            "glac",
            "--name",
            "Glaciology",
            "--path",
            root,
        ])
        .unwrap();
        site(&["project", "archive", "glac", "--path", root]).unwrap();
        site(&["project", "list", root]).unwrap();
        site(&["project", "unarchive", "glac", "--path", root]).unwrap();
        let refused = site(&["project", "delete", "glac", "--path", root]).unwrap_err();
        assert!(refused.contains("Archive it first"), "{refused}");

        // An archived project takes no new people.
        site(&["project", "archive", "glac", "--path", root]).unwrap();
        let refused = site(&[
            "account",
            "add-bulk",
            "--count",
            "1",
            "--project",
            "glac",
            "--path",
            root,
        ])
        .unwrap_err();
        assert!(refused.contains("archived"), "{refused}");
        site(&["project", "unarchive", "glac", "--path", root]).unwrap();

        // Removing an account takes its membership with it.
        let opened = crate::site::Site::open(dir.path()).unwrap();
        let project = opened.project(&key("glac")).unwrap();
        crate::project::members::update(project.documents(), |set| {
            set.upsert(
                &name("bo"),
                crate::project::roles::Role::Picker,
                crate::project::roles::DownloadScope::All,
            );
            Ok(())
        })
        .unwrap();
        super::project_info_command(&ProjectInfoArgs {
            path: project.root().to_path_buf(),
        })
        .unwrap();
        site(&["account", "remove", "bo", "--path", root]).unwrap();
        let (members, _) = crate::project::members::read(project.documents())
            .unwrap()
            .unwrap();
        assert!(members.get(&name("bo")).is_none());
        assert!(site(&["account", "remove", "bo", "--path", root]).is_err());

        site(&["project", "archive", "glac", "--path", root]).unwrap();
        site(&["project", "delete", "glac", "--path", root]).unwrap();
        assert!(!dir.path().join("projects/glac").exists());

        // Every change above is in the history, attributed to the command line.
        let log = crate::site::audit::read(opened.store()).unwrap();
        assert!(log.entries.len() >= 10, "{:?}", log.entries);
        assert!(log.entries.iter().all(|entry| entry.actor == "cli"));
    }

    #[test]
    fn a_site_command_outside_a_site_says_how_to_make_one() {
        let dir = tempfile::tempdir().unwrap();
        let refused = site(&["account", "list", dir.path().to_str().unwrap()]).unwrap_err();
        assert!(refused.contains("ridal site init"), "{refused}");
    }

    #[test]
    fn a_batch_cannot_be_a_sites_first_accounts() {
        let dir = tempfile::tempdir().unwrap();
        crate::site::Site::init(dir.path(), None).unwrap();
        let refused = add_bulk(dir.path(), &["--count", "2"]).unwrap_err();
        assert!(refused.contains("--server-admin"), "{refused}");
    }

    #[cfg(feature = "server")]
    #[test]
    fn gui_does_not_open_a_browser_unless_asked() {
        // Opening a browser by default under `ssh`, a container, or any
        // environment without a display is a regression (#200); the URL is
        // printed either way, and `--open-browser` is opt-in.
        let args = Args::try_parse_from(["ridal", "gui"]).unwrap();
        match args.command {
            Commands::Gui(gui) => assert!(!gui.open_browser),
            other => panic!("expected the gui command, got {other:?}"),
        }

        let args = Args::try_parse_from(["ridal", "gui", "--open-browser"]).unwrap();
        match args.command {
            Commands::Gui(gui) => assert!(gui.open_browser),
            other => panic!("expected the gui command, got {other:?}"),
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn gui_takes_any_free_port_unless_given_one() {
        let args = Args::try_parse_from(["ridal", "gui"]).unwrap();
        match args.command {
            Commands::Gui(gui) => assert_eq!(gui.port, None),
            other => panic!("expected the gui command, got {other:?}"),
        }

        let args = Args::try_parse_from(["ridal", "gui", "--port", "8765"]).unwrap();
        match args.command {
            Commands::Gui(gui) => assert_eq!(gui.port, Some(8765)),
            other => panic!("expected the gui command, got {other:?}"),
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn n_workers_zero_is_rejected_not_silently_clamped() {
        // n_workers sizes the render permit semaphore; zero permits would
        // leave every image request waiting until it times out into a 503,
        // which is a confusing way to learn about a typo.
        let err = super::render_service_config(None, Some(0)).unwrap_err();
        assert!(err.contains("--n-workers"), "{err}");

        assert!(super::render_service_config(None, Some(1)).is_ok());
        assert_eq!(
            super::render_service_config(None, None).unwrap().n_workers,
            crate::server::render_service::RenderServiceConfig::default().n_workers,
            "an omitted flag must keep the default, not become an error"
        );
    }

    #[test]
    fn test_parse_process_command() {
        let args = Args::parse_from([
            "ridal",
            "process",
            "a.rad",
            "b.rad",
            "--default",
            "-o",
            "out.nc",
        ]);
        match args.command {
            Commands::Process(process) => {
                assert_eq!(
                    process.inputs,
                    vec![PathBuf::from("a.rad"), PathBuf::from("b.rad")]
                );
                assert!(process.default);
                assert_eq!(process.output, Some(PathBuf::from("out.nc")));
            }
            _ => panic!("Expected process command"),
        }
    }

    #[test]
    fn test_parse_info_command() {
        let args = Args::parse_from(["ridal", "info", "line01.rad", "--json"]);
        match args.command {
            Commands::Info(info) => {
                assert_eq!(info.inputs, vec![PathBuf::from("line01.rad")]);
                assert!(info.json);
            }
            _ => panic!("Expected info command"),
        }
    }

    #[test]
    fn test_parse_override_antenna_separation() {
        let args = Args::parse_from([
            "ridal",
            "process",
            "line01.DZT",
            "--override-antenna-separation",
            "1.25",
        ]);
        match args.command {
            Commands::Process(process) => {
                assert_eq!(process.override_antenna_separation, Some(1.25));
            }
            _ => panic!("Expected process command"),
        }
    }

    #[test]
    fn test_choose_steps_default_with_topo() {
        let steps = choose_steps(false, true, None).unwrap();
        assert!(steps.iter().any(|step| step == "correct_topography"));
    }
}

/// Parse the `--spacing` value.
///
/// Accepts a bare number of metres, "auto", or "per-trace". A bare number is
/// metres rather than traces by design: see [`InterpExportArgs::spacing`].
pub fn parse_spacing(text: &str) -> Result<crate::interp::level2::Spacing, String> {
    use crate::interp::level2::Spacing;
    match text.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(Spacing::Auto),
        "per-trace" | "per_trace" | "pertrace" => Ok(Spacing::PerTrace),
        "vertices" | "vertex" => Ok(Spacing::Vertices),
        other => {
            // Tolerate a trailing "m" so `--spacing 5m` does not fail on
            // something that obviously means five metres.
            let numeric = other.strip_suffix('m').unwrap_or(other);
            let step: f64 = numeric.parse().map_err(|_| {
                format!(
                    "Could not read --spacing '{text}'. Expected a distance in metres \
                     (e.g. '5' or '2.5'), 'auto', 'per-trace', or 'vertices'."
                )
            })?;
            if !step.is_finite() || step <= 0.0 {
                return Err(format!("--spacing must be greater than zero, got '{text}'"));
            }
            Ok(Spacing::ArcLength(step))
        }
    }
}

fn interp_export_command(args: &InterpExportArgs) -> Result<(), String> {
    let spacing = parse_spacing(&args.spacing)?;

    let text = std::fs::read_to_string(&args.interpretation)
        .map_err(|e| format!("Could not read {:?}: {e}", args.interpretation))?;
    let document = gprinterp::Document::from_json(&text).map_err(|e| {
        format!(
            "Could not parse {:?} as gprinterp: {e}",
            args.interpretation
        )
    })?;

    // Validation warnings are surfaced but not fatal: the format is
    // deliberately permissive, and a document missing a stable feature id
    // still exports correctly.
    let report = gprinterp::validate(&document);
    if !report.errors.is_empty() {
        // All of them, not just the first: a hand-written document usually
        // has several problems at once, and fixing them one round trip at a
        // time is needless.
        let errors: Vec<String> = report.errors.iter().map(|e| format!("  - {e}")).collect();
        return Err(format!(
            "{:?} is not a valid gprinterp document:\n{}",
            args.interpretation,
            errors.join("\n")
        ));
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }

    let geometry = crate::interp::source::read_geometry(&args.radargram)?;

    // Shared with the HTTP download routes, which previously skipped this
    // and produced a plausible, wrong file from the same inputs.
    let identity = crate::interp::checks::check_identity(&document, &geometry)
        .map_err(|e| format!("{:?}: {e}", args.interpretation))?;
    if let Some(warning) = identity.warning {
        eprintln!("warning: {warning}");
    }

    // Overhang permission is a property of the project's layer vocabulary.
    // An export from outside a project has no vocabulary, so nothing has
    // opted out and the guardrail applies everywhere -- the safe default.
    let layer_set =
        match crate::project::Project::discover(&args.radargram).map_err(|e| e.to_string())? {
            Some(project) => {
                crate::project::layers::read(project.documents())
                    .map_err(|e| e.to_string())?
                    .0
            }
            None => crate::project::layers::LayerSet::default(),
        };
    let allows_overhangs = |label: Option<&str>| layer_set.allows_overhangs(label);

    let export =
        crate::interp::level2::export(&document, &geometry, spacing, &args.user, &allows_overhangs)
            .map_err(|e| format!("{e}"))?;

    let output_crs = match &args.crs {
        None => crate::interp::writer::OutputCrs::Wgs84,
        Some(name) => crate::interp::writer::OutputCrs::Named(name.clone()),
    };

    let extension = args
        .output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // A slice of one: the writers take several so a group download can
    // concatenate them, and a single export is that with one element.
    let exports = [export];
    let serialized = match extension.as_str() {
        "csv" => crate::interp::writer::to_csv(&exports),
        "geojson" | "json" => crate::interp::writer::to_geojson(&exports, &output_crs)?,
        other => {
            return Err(format!(
                "Cannot tell what format to write from the extension '{other}'. \
                 Use '.geojson' or '.csv'."
            ))
        }
    };
    if extension == "csv" && args.crs.is_some() {
        eprintln!(
            "warning: --crs is ignored for CSV output, which always carries both native \
             easting/northing and WGS84 longitude/latitude as columns."
        );
    }

    std::fs::write(&args.output, serialized)
        .map_err(|e| format!("Could not write {:?}: {e}", args.output))?;

    let export = &exports[0];
    let spacing_note = match export.spacing_m {
        Some(step) => format!("{step} m spacing"),
        None => "per-trace spacing".to_string(),
    };
    println!(
        "Wrote {} point(s) from {} layer(s) at {spacing_note} to {:?}",
        export.points.len(),
        document.layers().len(),
        args.output
    );
    Ok(())
}

fn project_init_command(args: &ProjectInitArgs) -> Result<(), String> {
    let project = crate::project::Project::init(&args.path, args.name.as_deref())
        .map_err(|e| e.to_string())?;
    println!("Created project at {}", project.root().display());
    println!(
        "  {} names it and holds its settings; everything else Ridal owns is in {}/.",
        crate::project::MARKER,
        crate::project::DEFAULT_DATA_DIR,
    );
    println!(
        "  Interpretations go in {0}/{1}/, layer definitions in {0}/{2}/, \
         derived data in {0}/{3}/.",
        crate::project::DEFAULT_DATA_DIR,
        crate::project::INTERPRETATIONS_DIR,
        crate::project::LAYERS_DIR,
        crate::project::DEFAULT_CACHE_DIR,
    );
    println!(
        "  Your own files stay where they are; uploads from the browser go to {}/.",
        project.relative_upload_dir().display()
    );
    Ok(())
}

fn project_migrate_command(args: &ProjectMigrateArgs) -> Result<(), String> {
    let plan = crate::project::migrate::plan(&args.path).map_err(|e| e.to_string())?;
    if plan.is_empty() {
        println!(
            "{} is already in the current layout; there is nothing to move.",
            plan.root.display()
        );
        return Ok(());
    }

    let verb = if args.dry_run { "Would move" } else { "Moved" };
    if !args.dry_run {
        crate::project::migrate::apply(&plan).map_err(|e| e.to_string())?;
    }
    for entry in &plan.moves {
        println!("{verb} {entry} -> {}/{entry}", plan.data_dir.display());
    }
    if plan.write_gitignore {
        println!(
            "{} {}/.gitignore",
            if args.dry_run { "Would write" } else { "Wrote" },
            plan.data_dir.display()
        );
    }
    if args.dry_run {
        println!("Nothing was changed. Run without --dry-run to do it.");
    } else {
        println!(
            "{} is now a format_version {} project.",
            plan.root.display(),
            crate::project::FORMAT_VERSION
        );
    }
    Ok(())
}

fn project_info_command(args: &ProjectInfoArgs) -> Result<(), String> {
    let Some(project) = crate::project::Project::discover(&args.path).map_err(|e| e.to_string())?
    else {
        return Err(format!(
            "No Ridal project at or above {}. Run `ridal project init` to create one.",
            args.path.display()
        ));
    };

    println!("Project: {}", project.root().display());
    if let Some(name) = &project.config().project.name {
        println!("Name: {name}");
    }
    println!("Data: {}", project.data_dir().display());
    for root in project.radargram_roots() {
        println!("Radargram root: {}", root.display());
    }
    println!("Cache: {}", project.cache_dir().display());
    // Both halves of the project layer of the preference cascade, so
    // "why does it open like that?" is answerable without the browser.
    println!(
        "Default render profile: {}",
        project
            .default_profile()
            .unwrap_or_else(|| "(unset, Ridal's built-in default)".to_string())
    );
    println!(
        "Default horizontal scale: {}",
        match project.default_xscale() {
            Some(scale) => format!("{scale}x"),
            None => "(unset, 1x)".to_string(),
        }
    );

    let (layers, _) =
        crate::project::layers::read(project.documents()).map_err(|e| e.to_string())?;
    println!("Layers: {}", layers.layers.len());
    for layer in &layers.layers {
        println!("  {} ({})", layer.id, layer.name);
    }
    // An exclusivity group that names a layer nobody defined still removes
    // values at evaluation time, so it is worth saying out loud -- the same
    // reason undefined labels are reported below.
    for warning in layers.group_warnings() {
        println!("  warning: {warning}");
    }

    // Listed from the interpretations directory rather than from the
    // catalog: an interpretation whose radargram is missing is exactly the
    // thing worth noticing, and inspecting must not need the server feature.
    let interpretations = project.data_dir().join(crate::project::INTERPRETATIONS_DIR);
    let mut total = 0usize;
    if let Ok(entries) = std::fs::read_dir(&interpretations) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for name in names {
            let Ok(radargram) = crate::identity::RadargramId::new(&name) else {
                continue;
            };
            let users =
                crate::project::interpretations::list_users(project.documents(), &radargram)
                    .map_err(|e| e.to_string())?;
            if !users.is_empty() {
                println!("Interpretations: {name} <- {}", users.join(", "));
                total += users.len();
            }
            for user in &users {
                let Ok(user_id) = crate::identity::UserId::new(user.as_str()) else {
                    continue;
                };
                let Some(stored) = crate::project::interpretations::read(
                    project.documents(),
                    &radargram,
                    &user_id,
                )
                .map_err(|e| e.to_string())?
                else {
                    continue;
                };
                // Labels with no definition are worth surfacing: the picks
                // are real, the vocabulary just does not describe them, and
                // the viewer will draw them with no colour.
                let labels = stored.document.features.iter().filter_map(|f| f.label());
                let unknown = layers.unknown_ids(labels);
                if !unknown.is_empty() {
                    println!(
                        "  warning: {name}/{user} uses undefined layer(s): {}",
                        unknown.join(", ")
                    );
                }
            }
        }
    }
    if total == 0 {
        println!("Interpretations: none yet");
    }

    // Who may use it inside a site. Memberships name site accounts, so a
    // project on its own (`ridal gui`) has only its one local person.
    match crate::project::members::read(project.documents()).map_err(|e| e.to_string())? {
        Some((set, _)) => {
            println!("Members (when served by a site): {}", set.members.len());
            for member in &set.members {
                println!(
                    "  {} ({}, downloads: {})",
                    member.name, member.role, member.download
                );
            }
            println!(
                "Public read: {}",
                if set.require_auth_to_read {
                    "no, a login is required"
                } else {
                    "yes"
                }
            );
        }
        None => println!("Members: none"),
    }
    Ok(())
}

/// Print an invite link the way it can actually be used.
///
/// The path, plus an example of what to prefix it with. This command has no
/// idea what address the server will be reached on -- it may not even be
/// running -- so inventing a hostname would be inventing one, and a link
/// that looks authoritative and is wrong is worse than one that is
/// obviously a fragment.
fn print_invite(name: &str, token: &str, expires: i64) {
    let days = crate::site::accounts::invite::INVITE_TTL_DAYS;
    println!("Invite link for '{name}' (valid {days} days, single use):");
    println!("  /invite/{token}");
    println!("Prefix it with the address the server is reached on, e.g.");
    println!("  http://localhost:8000/invite/{token}");
    if let Some(when) = chrono::DateTime::from_timestamp(expires, 0) {
        println!("Expires {}", when.format("%Y-%m-%d %H:%M UTC"));
    }
    println!();
    println!(
        "This is the only time it is shown -- only its hash is stored. It is as \
         sensitive as a password until it is used or expires, so send it the way \
         you would send one."
    );
}

fn site_command(args: SiteArgs) -> Result<(), String> {
    match args.command {
        SiteCommand::Init(args) => site_init_command(&args),
        SiteCommand::Account(args) => match args.command {
            SiteAccountCommand::Add(args) => site_account_add_command(&args),
            SiteAccountCommand::AddBulk(args) => site_account_add_bulk_command(&args),
            SiteAccountCommand::List(args) => site_account_list_command(&args),
            SiteAccountCommand::Set(args) => site_account_set_command(&args),
            SiteAccountCommand::Reset(args) => site_account_reset_command(&args),
            SiteAccountCommand::Remove(args) => site_account_remove_command(&args),
        },
        SiteCommand::Project(args) => match args.command {
            SiteProjectCommand::Add(args) => site_project_add_command(&args),
            SiteProjectCommand::List(args) => site_project_list_command(&args),
            SiteProjectCommand::Archive(args) => site_project_archive_command(&args, true),
            SiteProjectCommand::Unarchive(args) => site_project_archive_command(&args, false),
            SiteProjectCommand::Delete(args) => site_project_delete_command(&args),
        },
        SiteCommand::Token(args) => match args.command {
            SiteTokenCommand::Add(args) => site_token_add_command(&args),
            SiteTokenCommand::List(args) => site_token_list_command(&args),
            SiteTokenCommand::Revoke(args) => site_token_revoke_command(&args),
        },
    }
}

/// `PROJECT:ROLE[:DOWNLOAD]`, with the membership's scope when none is given.
fn parse_token_grant(
    site: &crate::site::Site,
    account: &crate::site::accounts::Account,
    text: &str,
) -> Result<crate::site::tokens::Grant, String> {
    use crate::project::roles::{DownloadScope, Role};
    let mut parts = text.split(':');
    let (Some(key), Some(role)) = (parts.next(), parts.next()) else {
        return Err(format!(
            "'{text}' is not a grant. Write it as PROJECT:ROLE or PROJECT:ROLE:DOWNLOAD, \
             such as glac:operator."
        ));
    };
    let project = crate::identity::ProjectKey::new(key)?;
    let role = Role::parse(role)?;
    let download = match parts.next() {
        Some(scope) => DownloadScope::parse(scope)?,
        None => crate::site::tokens::widest_download(site, account, &project)
            .unwrap_or(DownloadScope::None),
    };
    if parts.next().is_some() {
        return Err(format!("'{text}' has too many parts for a grant."));
    }
    Ok(crate::site::tokens::Grant {
        project,
        role,
        download,
    })
}

fn site_token_add_command(args: &SiteTokenAddArgs) -> Result<(), String> {
    use crate::site::tokens;
    let site = open_site(&args.path)?;
    let name = crate::identity::UserId::new(args.account.clone())?;
    let accounts = crate::site::accounts::read(site.store())
        .map_err(|e| e.to_string())?
        .map(|(set, _)| set)
        .unwrap_or_default();
    let account = accounts
        .get(&name)
        .ok_or_else(|| format!("No account named '{name}'."))?;
    let grants = args
        .grants
        .iter()
        .map(|text| parse_token_grant(&site, account, text))
        .collect::<Result<Vec<_>, _>>()?;
    tokens::check_grants(&site, account, &grants).map_err(|e| e.to_string())?;
    let lifetime = tokens::parse_lifetime(&args.expires).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    let (text, token) =
        tokens::mint(now, name.clone(), &args.name, grants, lifetime).map_err(|e| e.to_string())?;
    tokens::update(site.store(), |set| {
        set.tokens.push(token.clone());
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::TokenCreated,
            name.as_str(),
        )
        .note(token.describe()),
    );
    println!("Created {}", token.describe());
    println!();
    println!("{text}");
    println!();
    println!("This is the only time it is shown. Send it as `Authorization: Bearer <token>`.");
    Ok(())
}

fn site_token_list_command(args: &SiteTokenListArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let only = args
        .account
        .clone()
        .map(crate::identity::UserId::new)
        .transpose()?;
    let (set, _) = crate::site::tokens::read(site.store()).map_err(|e| e.to_string())?;
    let listed: Vec<_> = set
        .tokens
        .iter()
        .filter(|token| only.is_none() || token.account == only)
        .collect();
    if listed.is_empty() {
        println!("No tokens.");
        return Ok(());
    }
    for token in listed {
        let holder = token
            .account
            .as_ref()
            .map_or("(no account)", |name| name.as_str());
        let expires = match token.expires {
            Some(at) => chrono::DateTime::from_timestamp(at, 0)
                .map(|at| format!("expires {}", at.format("%Y-%m-%d")))
                .unwrap_or_default(),
            None => "NEVER EXPIRES".to_string(),
        };
        println!("{holder}: {} [{expires}]", token.describe());
    }
    Ok(())
}

fn site_token_revoke_command(args: &SiteTokenRevokeArgs) -> Result<(), String> {
    use crate::site::tokens;
    let site = open_site(&args.path)?;
    let revoked = tokens::update(site.store(), |set| {
        let index = set
            .tokens
            .iter()
            .position(|token| token.id == args.id)
            .ok_or_else(|| tokens::TokenError::NotFound(args.id.clone()))?;
        Ok(set.tokens.remove(index))
    })
    .map_err(|e| e.to_string())?;
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::TokenRevoked,
            revoked.account.as_ref().map_or("", |name| name.as_str()),
        )
        .note(revoked.describe()),
    );
    println!("Revoked {}", revoked.describe());
    Ok(())
}

/// Open the site containing `path`, or say how to make one.
fn open_site(path: &std::path::Path) -> Result<crate::site::Site, String> {
    crate::site::Site::discover(path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "No Ridal site at or above {}. Run `ridal site init` to create one.",
                path.display()
            )
        })
}

fn site_init_command(args: &SiteInitArgs) -> Result<(), String> {
    let site =
        crate::site::Site::init(&args.path, args.name.as_deref()).map_err(|e| e.to_string())?;
    println!("Created a Ridal site at {}", site.root().display());
    println!(
        "Next, create a server administrator:\n  ridal site account add <name> --server-admin"
    );
    Ok(())
}

fn site_account_add_command(args: &SiteAccountAddArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let name = crate::identity::UserId::new(args.name.clone())?;
    let (token, invite) =
        crate::site::accounts::invite::mint(chrono::Utc::now().timestamp(), None, None, None)?;
    crate::site::accounts::update(site.store(), |set| {
        use crate::site::accounts::AccountError;
        if set.get(&name).is_some() {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        // An account that cannot administer the site would leave nobody
        // able to create projects or accounts. The first account is the
        // administrator's, exactly as the first project account was.
        if !args.server_admin && !set.has_server_admin() {
            return Err(AccountError::Rejected(format!(
                "'{name}' would be the first account, and without --server-admin it \
                 could not create projects or accounts. Create an administrator \
                 first:\n  ridal site account add {name} --server-admin"
            )));
        }
        let mut account = crate::site::accounts::Account::new(name.clone(), args.server_admin);
        account.invite = Some(invite.clone());
        set.users.push(account);
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    let mut entry = crate::site::audit::Entry::new(
        "cli",
        crate::site::audit::Action::AccountCreated,
        name.as_str(),
    );
    if args.server_admin {
        entry = entry.note("server administrator");
    }
    crate::site::audit::record(site.store(), entry);
    print_invite(name.as_str(), &token, invite.expires);
    Ok(())
}

fn site_account_add_bulk_command(args: &SiteAccountAddBulkArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let role = crate::project::roles::Role::parse(&args.role)?;
    let download = crate::project::roles::DownloadScope::parse(&args.download)?;
    let project = match args.project.as_deref() {
        Some(raw) => {
            let key = crate::identity::ProjectKey::new(raw)?;
            // Opened, not merely checked for: a project the site would refuse
            // to serve is no place to add members.
            site.project(&key).map_err(|e| e.to_string())?;
            if site.is_archived(&key) {
                return Err(format!(
                    "Project '{key}' is archived. Unarchive it before adding people to it."
                ));
            }
            Some(key)
        }
        None => None,
    };
    if args.passwords && !args.i_know_what_i_am_doing {
        return Err(
            "Generated passwords are shared secrets. Re-run with --i-know-what-i-am-doing, \
             or leave out --passwords to use invite links instead."
                .to_string(),
        );
    }
    // Like `site account add`: a batch cannot be the site's first accounts,
    // because none of them would be able to administer it.
    let has_admin = crate::site::accounts::read(site.store())
        .map_err(|e| e.to_string())?
        .is_some_and(|(set, _)| set.has_server_admin());
    if !has_admin {
        return Err("Create a server administrator first:\n  \
                    ridal site account add <name> --server-admin"
            .to_string());
    }

    let prefix = (!args.random_names).then_some(args.prefix.as_str());
    let names = site
        .batch_names(prefix, args.count)
        .map_err(|e| e.to_string())?;
    let grant = crate::site::Grant {
        project: project.clone(),
        role,
        download,
    };
    let record = |name: &crate::identity::UserId, note: &str| {
        let mut entry = crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::AccountCreated,
            name.as_str(),
        )
        .note(note);
        if let Some(key) = &project {
            entry = entry.project(key).membership(role, download);
        }
        crate::site::audit::record(site.store(), entry);
    };

    if args.passwords {
        #[cfg(not(feature = "server"))]
        return Err("Password mode needs a build with the server feature.".to_string());

        #[cfg(feature = "server")]
        {
            let generated = site
                .password_batch(&names, &grant)
                .map_err(|e| e.to_string())?;
            // Written to a file rather than echoed: a terminal is a log, and
            // standard output is routinely captured. The file is the
            // handout, and the operator deletes it after distributing.
            let mut handout = String::new();
            for (name, password) in &generated {
                record(name, "bulk generated password");
                handout.push_str(&format!("{name}\t{password}\n"));
            }
            std::fs::write(&args.out, handout)
                .map_err(|e| format!("could not write {}: {e}", args.out.display()))?;
            if let Some(advisory) = crate::site::accounts::bulk::bulk_risk_advisory(role) {
                println!("{advisory}");
            }
            println!(
                "Wrote {} generated passwords to {}.",
                generated.len(),
                args.out.display()
            );
            println!(
                "Hand them out, then delete that file: it is as sensitive as the \
                 passwords themselves and is not stored anywhere else."
            );
            return Ok(());
        }
    }

    let minted = site
        .invite_batch(&names, &grant)
        .map_err(|e| e.to_string())?;
    println!("Invite links let each person set their own password.");
    for (name, token, expires) in &minted {
        record(name, "bulk invite");
        print_invite(name.as_str(), token, *expires);
    }
    Ok(())
}

fn site_account_list_command(args: &SiteAccountListArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let Some((set, _version)) =
        crate::site::accounts::read(site.store()).map_err(|e| e.to_string())?
    else {
        println!(
            "No accounts yet. Create one with `ridal site account add <name> --server-admin`."
        );
        return Ok(());
    };
    if set.users.is_empty() {
        println!("No accounts.");
        return Ok(());
    }
    // `person` rather than `account`: CodeQL's cleartext-logging query reads
    // a variable called `account` as sensitive by its name, and what is
    // printed here is only the name and what state the account is in.
    for person in &set.users {
        let admin = if person.server_admin {
            " [server admin]"
        } else {
            ""
        };
        let state = if person.is_activated() {
            "active"
        } else if person.invite.is_some() {
            "invite pending"
        } else {
            "no password"
        };
        println!("{}{} ({state})", person.name, admin);
    }
    Ok(())
}

fn site_account_set_command(args: &SiteAccountSetArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let name = crate::identity::UserId::new(args.name.clone())?;
    let wanted = if args.no_server_admin {
        false
    } else if args.server_admin {
        true
    } else {
        return Err("Pass --server-admin or --no-server-admin.".to_string());
    };
    crate::site::accounts::update(site.store(), |set| {
        use crate::site::accounts::AccountError;
        // The last administrator cannot demote themselves, or the site has
        // nobody who can create projects or accounts.
        if !wanted
            && !set.has_another_admin(&name)
            && set.get(&name).is_some_and(|account| account.server_admin)
        {
            return Err(AccountError::Rejected(
                "This is the only server administrator. Grant \
                 --server-admin to somebody else first."
                    .to_string(),
            ));
        }
        let account = set
            .get_mut(&name)
            .ok_or_else(|| AccountError::NotFound(name.to_string()))?;
        account.server_admin = wanted;
        // A live session must not keep authority it no longer has.
        account.credential_version += 1;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            if wanted {
                crate::site::audit::Action::ServerAdminGranted
            } else {
                crate::site::audit::Action::ServerAdminRevoked
            },
            name.as_str(),
        ),
    );
    Ok(())
}

fn site_account_reset_command(args: &SiteAccountResetArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let name = crate::identity::UserId::new(args.name.clone())?;
    let (token, invite) =
        crate::site::accounts::invite::mint(chrono::Utc::now().timestamp(), None, None, None)?;
    crate::site::accounts::update(site.store(), |set| {
        use crate::site::accounts::AccountError;
        let account = set
            .get_mut(&name)
            .ok_or_else(|| AccountError::NotFound(name.to_string()))?;
        account.invite = Some(invite.clone());
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::InviteIssued,
            name.as_str(),
        ),
    );
    print_invite(name.as_str(), &token, invite.expires);
    Ok(())
}

fn site_account_remove_command(args: &SiteAccountResetArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let name = crate::identity::UserId::new(args.name.clone())?;
    let removed_from = site.remove_account(&name).map_err(|e| e.to_string())?;
    for key in &removed_from {
        crate::site::audit::record(
            site.store(),
            crate::site::audit::Entry::new(
                "cli",
                crate::site::audit::Action::MembershipRemoved,
                name.as_str(),
            )
            .project(key)
            .note("account removed"),
        );
    }
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::AccountRemoved,
            name.as_str(),
        ),
    );
    println!("Removed account '{name}'.");
    Ok(())
}

fn site_project_add_command(args: &SiteProjectAddArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let key = crate::identity::ProjectKey::new(args.key.clone())?;
    let project = site
        .create_project(&key, args.name.as_deref(), None)
        .map_err(|e| e.to_string())?;
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::ProjectCreated,
            key.as_str(),
        ),
    );
    println!("Created project '{}' at {}", key, project.root().display());
    Ok(())
}

fn site_project_list_command(args: &SiteProjectListArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let keys = site.list().map_err(|e| e.to_string())?;
    if keys.is_empty() {
        println!("No projects yet. Create one with `ridal site project add <key>`.");
        return Ok(());
    }
    for key in keys {
        let name = site
            .project(&key)
            .ok()
            .and_then(|project| project.config().project.name.clone())
            .unwrap_or_else(|| key.to_string());
        let archived = if site.is_archived(&key) {
            " [archived]"
        } else {
            ""
        };
        println!("{key}  {name}{archived}");
    }
    Ok(())
}

fn site_project_archive_command(args: &SiteProjectKeyArgs, archive: bool) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let key = crate::identity::ProjectKey::new(args.key.clone())?;
    if archive {
        site.archive(&key).map_err(|e| e.to_string())?;
        crate::site::audit::record(
            site.store(),
            crate::site::audit::Entry::new(
                "cli",
                crate::site::audit::Action::ProjectArchived,
                key.as_str(),
            ),
        );
        println!("Archived '{key}' (read-only).");
    } else {
        site.unarchive(&key).map_err(|e| e.to_string())?;
        crate::site::audit::record(
            site.store(),
            crate::site::audit::Entry::new(
                "cli",
                crate::site::audit::Action::ProjectUnarchived,
                key.as_str(),
            ),
        );
        println!("Unarchived '{key}'.");
    }
    Ok(())
}

fn site_project_delete_command(args: &SiteProjectKeyArgs) -> Result<(), String> {
    let site = open_site(&args.path)?;
    let key = crate::identity::ProjectKey::new(args.key.clone())?;
    site.delete_project(&key).map_err(|e| e.to_string())?;
    crate::site::audit::record(
        site.store(),
        crate::site::audit::Entry::new(
            "cli",
            crate::site::audit::Action::ProjectDeleted,
            key.as_str(),
        ),
    );
    println!("Deleted project '{key}'.");
    Ok(())
}

/// The CLI reference in the documentation, generated from these definitions.
///
/// Only with `server`, so that `gui` and `server` are in it: the page
/// documents the full CLI, which is what `cargo install ridal` builds.
#[cfg(all(test, feature = "server"))]
mod reference_tests {
    use clap::CommandFactory;
    use std::fmt::Write;

    fn markdown() -> String {
        let mut command = super::Args::command();
        command.build();
        let mut out = String::from(
            "<!-- Generated from src/cli.rs; edit the doc comments there, then run\n     \
             UPDATE_CLI_MD=1 cargo test --no-default-features -F cli,server cli_md -->\n\n\
             # CLI reference\n\n\
             Every command and option of `ridal`, generated from the program itself. \
             `ridal <command> --help` prints the same text.\n",
        );
        for sub in command.get_subcommands() {
            write_command(&mut out, sub, 2);
        }
        out
    }

    fn write_command(out: &mut String, command: &clap::Command, depth: usize) {
        // clap's generated `help` subcommand only repeats `--help`.
        if command.is_hide_set() || command.get_name() == "help" {
            return;
        }
        let name = command.get_bin_name().unwrap_or(command.get_name());
        let heading = "#".repeat(depth.min(4));
        // `program` scopes the `option` entries below to this command, so
        // `--quiet` on two commands are two entries, each linkable as
        // {option}`ridal render --quiet`.
        write!(
            out,
            "\n{heading} `{name}`\n\n```{{program}} {name}\n```\n\n"
        )
        .unwrap();
        if let Some(about) = command.get_long_about().or(command.get_about()) {
            write!(out, "{}\n\n", about.to_string().trim()).unwrap();
        }
        let usage = command.clone().render_usage().to_string();
        let usage = usage.trim().trim_start_matches("Usage:").trim();
        write!(out, "```console\n$ {usage}\n```\n").unwrap();

        let arguments: Vec<&clap::Arg> = command
            .get_arguments()
            .filter(|arg| !arg.is_hide_set())
            .filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"))
            .collect();
        for (title, positional) in [("Arguments", true), ("Options", false)] {
            let selected: Vec<_> = arguments
                .iter()
                .filter(|arg| arg.is_positional() == positional)
                .collect();
            if selected.is_empty() {
                continue;
            }
            write!(out, "\n**{title}**\n").unwrap();
            for arg in selected {
                write_argument(out, arg);
            }
        }
        for sub in command.get_subcommands() {
            write_command(out, sub, depth + 1);
        }
    }

    fn write_argument(out: &mut String, arg: &clap::Arg) {
        let value = arg
            .get_value_names()
            .map(|names| {
                names
                    .iter()
                    .map(|name| format!("<{name}>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let takes_value = arg.get_num_args().is_some_and(|n| n.takes_values());
        // The forms Sphinx's `option` directive parses: `-o <OUTPUT>, --output
        // <OUTPUT>` for an option, and the bare `<INPUTS>` for a positional.
        let with_value = |flag: String| {
            if takes_value && !value.is_empty() {
                format!("{flag} {value}")
            } else {
                flag
            }
        };
        let mut term = Vec::new();
        if let Some(short) = arg.get_short() {
            term.push(with_value(format!("-{short}")));
        }
        if let Some(long) = arg.get_long() {
            term.push(with_value(format!("--{long}")));
        }
        if arg.is_positional() {
            term.push(value.clone());
        }
        write!(out, "\n```{{option}} {}\n", term.join(", ")).unwrap();

        let help = arg
            .get_long_help()
            .or(arg.get_help())
            .map(|help| help.to_string())
            .unwrap_or_default();
        let mut notes = Vec::new();
        if arg.is_required_set() {
            notes.push("Required.".to_string());
        }
        let defaults: Vec<_> = arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        if !defaults.is_empty() && takes_value {
            notes.push(format!("Default: `{}`.", defaults.join(" ")));
        }
        let possible: Vec<_> = arg
            .get_possible_values()
            .iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| format!("`{}`", value.get_name()))
            .collect();
        if !possible.is_empty() && takes_value {
            notes.push(format!("One of {}.", possible.join(", ")));
        }
        let mut paragraphs: Vec<String> = help
            .trim()
            .split("\n\n")
            .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|paragraph| !paragraph.is_empty())
            .collect();
        if !notes.is_empty() {
            paragraphs.push(notes.join(" "));
        }
        if paragraphs.is_empty() {
            paragraphs.push("(Not documented.)".to_string());
        }
        write!(out, "{}\n```\n", paragraphs.join("\n\n")).unwrap();
    }

    #[test]
    fn cli_md_is_up_to_date() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/cli.md");
        let expected = markdown();
        if std::env::var_os("UPDATE_CLI_MD").is_some() {
            // CI must check the committed file, never regenerate it.
            assert!(
                std::env::var_os("CI").is_none(),
                "UPDATE_CLI_MD is set in CI"
            );
            std::fs::write(&path, &expected).unwrap();
        }
        // A Windows checkout may have converted the line endings.
        let actual = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            actual == expected,
            "docs/reference/cli.md is stale. Regenerate it with\n  \
             UPDATE_CLI_MD=1 cargo test --no-default-features -F cli,server cli_md"
        );
    }
}
