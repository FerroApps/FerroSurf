//! FerroSurf — a fast, modern, AI-native web browser powered by Servo.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app;
mod delegate;
mod input;
mod internal_pages;
mod mcp_host;
mod profile_data;
mod tab;
mod ui;
mod window;

use std::path::PathBuf;

use clap::Parser;

/// Command line interface.
#[derive(Parser, Debug, Clone, Default)]
#[command(
    name = "ferrosurf",
    version,
    about = "FerroSurf web browser (Servo engine)"
)]
pub struct Cli {
    /// URLs or search terms to open.
    pub urls: Vec<String>,

    /// Profile name or id to use.
    #[arg(long, short = 'P')]
    pub profile: Option<String>,

    /// Open a throw-away guest profile (nothing is saved).
    #[arg(long)]
    pub guest: bool,

    /// Show the profile picker at startup.
    #[arg(long)]
    pub profile_manager: bool,

    /// Open URLs in a new window of the running instance.
    #[arg(long)]
    pub new_window: bool,

    /// Run as an MCP stdio server that bridges to the running browser (for Claude Desktop & co).
    #[arg(long)]
    pub mcp_stdio: bool,

    /// Override the data directory (portable mode).
    #[arg(long, env = "FERROSURF_HOME")]
    pub data_dir: Option<PathBuf>,

    /// Load the first URL, wait until it's loaded and save a PNG screenshot of the window here, then exit.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,

    /// Extra seconds to wait before taking --screenshot.
    #[arg(long, default_value_t = 1.0)]
    pub screenshot_delay: f32,

    /// Initial window size, e.g. 1280x800.
    #[arg(long)]
    pub window_size: Option<String>,

    /// Load an unpacked extension from this folder (developer mode). Can be repeated.
    #[arg(long)]
    pub load_extension: Vec<PathBuf>,

    /// Don't restore the previous session.
    #[arg(long)]
    pub no_restore: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "warn,ferrosurf=info,ferrosurf_mcp=info,ferrosurf_core=info,ferrosurf_extensions=info",
    ))
    .init();

    let cli = Cli::parse();
    if let Some(dir) = &cli.data_dir {
        // SAFETY: called at startup before any other thread exists.
        unsafe { std::env::set_var("FERROSURF_HOME", dir) };
    }

    if cli.mcp_stdio {
        return run_mcp_stdio(&cli);
    }

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // If this profile is already open in another process, hand the URLs over to it.
    if !cli.guest && !cli.profile_manager && app::forward_to_running_instance(&cli) {
        return Ok(());
    }

    app::run(cli)
}

/// `ferrosurf --mcp-stdio`: proxy stdio JSON-RPC to the running browser's MCP endpoint.
fn run_mcp_stdio(cli: &Cli) -> anyhow::Result<()> {
    let profiles = ferrosurf_core::ProfileManager::open_default()?;
    let profile = match &cli.profile {
        Some(p) => profiles
            .get(p)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown profile {p}"))?,
        None => profiles.default_profile().clone(),
    };
    let endpoint_path = profiles.paths(&profile).mcp_token();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        // Wait for the browser to come up if it isn't running yet (start it if needed).
        let mut started = false;
        for attempt in 0..60 {
            if let Ok(ep) = ferrosurf_mcp::EndpointFile::read(&endpoint_path) {
                if app::pid_alive(ep.pid) {
                    return ferrosurf_mcp::run_stdio_bridge(&ep.url, &ep.token).await;
                }
            }
            if !started && attempt == 0 {
                started = true;
                let exe = std::env::current_exe()?;
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("--profile").arg(&profile.id);
                let _ = cmd.spawn();
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        anyhow::bail!("FerroSurf is not running and could not be started")
    })
}
