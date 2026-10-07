//! Map Editor's `dimos-app-server` (dimOS Desktop app contract, docs/apps.md in dimos-desktop): serves the built
//! page and every action as an HTTP endpoint (api.rs), listed in /agent.json for Desktop's agent. One compiled binary;
//! no Python or dimos at runtime.
mod api;
mod app;
mod desktop;
mod dimos_app;
mod lite_record;
mod persist;
mod probe;
mod routes;
mod session;
mod workspace;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(about = "Map Editor backend: build, clean, annotate and save maps from dimos recordings")]
pub struct Args {
    /// serve HTTP on this TCP port instead of DIMOS_APP's socket (development, tests)
    #[arg(long)]
    port: Option<u16>,
    /// the built page (vite's dist); the nix wrapper sets it
    #[arg(long, env = "MAP_BUILDER_FRONTEND")]
    frontend: Option<PathBuf>,
    /// print the endpoints (agent.json) and exit: scripts/check_endpoints.ts compares them with dimos.yaml
    #[arg(long)]
    agent_json: bool,
}

fn dimos_home() -> PathBuf {
    std::env::var("DIMOS_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".dimos"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // DIMOS_APP is the whole interface (docs/apps.md in dimos-desktop); run outside Desktop, the defaults below
    let app = dimos_app::get().cloned().unwrap_or_default();
    let desktop_url = app.desktop_url.clone().unwrap_or_default();
    if args.agent_json {
        println!("{}", serde_json::to_string_pretty(&api::routes().manifest(api::DESCRIPTION))?);
        return Ok(());
    }
    // where working sessions are autosaved, and Desktop's shared recordings folder
    let data = app.data_dir.map(PathBuf::from).unwrap_or_else(|| dimos_home().join("data").join("apps").join("dim-map-builder"));
    let recordings = app.recordings_dir.map(PathBuf::from).unwrap_or_else(|| dimos_home().join("recordings"));
    let frontend = args.frontend.clone().unwrap_or_else(|| PathBuf::from("frontend/dist"));
    eprintln!("map builder: page {}, sessions {}, recordings {}", frontend.display(), data.display(), recordings.display());
    let state = app::App::new(data, recordings);
    let _ = state.desktop_url.set(desktop_url.clone());
    match app.name {
        Some(name) if !desktop_url.is_empty() => desktop::spawn_relay(state.clone(), desktop_url, name),
        _ => eprintln!("map builder: no Desktop URL or app name (DIMOS_APP): events reach no page"),
    }
    let router = api::routes()
        .router
        .with_state(state.clone())
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
        let socket = app.socket.map(PathBuf::from).context("DIMOS_APP's socket (or --port) is required")?;
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
        axum::serve(listener, router).with_graceful_shutdown(shutdown).await?;
    }
    Ok(())
}
