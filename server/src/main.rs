//! Map Builder's `dimos-app-server` (dimOS Desktop app contract, docs/apps.md in dimos-desktop): serves the built
//! page, the editing API (/api), and the agent tools (/mcp). One compiled binary; no Python or dimos at runtime.
mod api;
mod app;
mod mcp;
mod persist;
mod session;
mod workspace;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(about = "Map Builder backend: build, clean, annotate and save maps from dimos recordings")]
pub struct Args {
    /// serve HTTP on this unix socket (Desktop passes it)
    #[arg(long, env = "DIMOS_APP_SOCKET")]
    socket: Option<PathBuf>,
    /// serve HTTP on this TCP port instead (development, tests)
    #[arg(long)]
    port: Option<u16>,
    #[arg(long, env = "DIMOS_DESKTOP_URL", default_value = "")]
    desktop_url: String,
    #[arg(long, env = "ZENOH_WEB_URL", default_value = "")]
    zenoh_web_url: String,
    #[arg(long, env = "ZENOH_CONNECT", default_value = "")]
    zenoh_connect: String,
    #[arg(long, env = "DIMOS_DIR", default_value = "")]
    dimos_dir: String,
    #[arg(long, env = "DIMOS_PYTHON", default_value = "")]
    dimos_python: String,
    /// the built page (vite's dist); the nix wrapper sets it
    #[arg(long, env = "MAP_BUILDER_FRONTEND")]
    frontend: Option<PathBuf>,
    /// where working sessions are autosaved [default: $DIMOS_APP_DATA, else ~/.dimos/data/apps/dim-map-builder]
    #[arg(long, env = "DIMOS_APP_DATA")]
    data: Option<PathBuf>,
    /// Desktop's shared recordings folder
    #[arg(long, env = "DIMOS_RECORDINGS_DIR")]
    recordings: Option<PathBuf>,
}

fn dimos_home() -> PathBuf {
    std::env::var("DIMOS_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".dimos"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let data = args.data.clone().unwrap_or_else(|| dimos_home().join("data").join("apps").join("dim-map-builder"));
    let recordings = args.recordings.clone().unwrap_or_else(|| dimos_home().join("recordings"));
    let frontend = args.frontend.clone().unwrap_or_else(|| PathBuf::from("frontend/dist"));
    eprintln!("map builder: page {}, sessions {}, recordings {}", frontend.display(), data.display(), recordings.display());
    let state = app::App::new(data, recordings);
    let router = api::router(state.clone())
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 << 20))
        .fallback_service(tower_http::services::ServeDir::new(&frontend).fallback(tower_http::services::ServeFile::new(frontend.join("index.html"))));
    let shutdown = async {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    };
    if let Some(port) = args.port {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("bind :{port}"))?;
        eprintln!("map builder: http://127.0.0.1:{port}/");
        axum::serve(listener, router).with_graceful_shutdown(shutdown).await?;
    } else {
        let socket = args.socket.clone().context("--socket or --port is required")?;
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
        axum::serve(listener, router).with_graceful_shutdown(shutdown).await?;
    }
    Ok(())
}
