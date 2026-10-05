//! Map Editor's `dimos-app-server` (dimOS Desktop app contract, docs/apps.md in dimos-desktop): serves the built
//! page and every action as an HTTP endpoint (api.rs), listed in /agent.json for Desktop's agent. One compiled binary;
//! no Python or dimos at runtime.
mod api;
mod app;
mod desktop;
mod dimos_app;
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
    /// serve HTTP on this unix socket (Desktop passes it in DIMOS_APP; these flags and env vars are the older
    /// Desktops' way, and DIMOS_APP wins over them)
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
    /// where working sessions are autosaved [default: DIMOS_APP's dataDir, else ~/.dimos/data/apps/dim-map-builder]
    #[arg(long, env = "DIMOS_APP_DATA")]
    data: Option<PathBuf>,
    /// Desktop's shared recordings folder
    #[arg(long, env = "DIMOS_RECORDINGS_DIR")]
    recordings: Option<PathBuf>,
    /// print the endpoints (agent.json) and exit: scripts/check_endpoints.ts compares them with dimos.yaml
    #[arg(long)]
    agent_json: bool,
}

fn dimos_home() -> PathBuf {
    std::env::var("DIMOS_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".dimos"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    if let Some(app) = dimos_app::get() {
        let set = |to: &mut String, from: &Option<String>| {
            if let Some(value) = from {
                *to = value.clone();
            }
        };
        let path = |from: &Option<String>| from.as_ref().map(PathBuf::from);
        args.socket = path(&app.socket).or(args.socket);
        args.data = path(&app.data_dir).or(args.data);
        args.recordings = path(&app.recordings_dir).or(args.recordings);
        set(&mut args.desktop_url, &app.desktop_url);
        set(&mut args.zenoh_web_url, &app.zenoh_web_url);
        set(&mut args.zenoh_connect, &app.zenoh_connect);
        set(&mut args.dimos_dir, &app.dimos_dir);
        set(&mut args.dimos_python, &app.dimos_python);
    }
    if args.agent_json {
        println!("{}", serde_json::to_string_pretty(&api::routes().manifest(api::DESCRIPTION))?);
        return Ok(());
    }
    let data = args.data.clone().unwrap_or_else(|| dimos_home().join("data").join("apps").join("dim-map-builder"));
    let recordings = args.recordings.clone().unwrap_or_else(|| dimos_home().join("recordings"));
    let frontend = args.frontend.clone().unwrap_or_else(|| PathBuf::from("frontend/dist"));
    eprintln!("map builder: page {}, sessions {}, recordings {}", frontend.display(), data.display(), recordings.display());
    let state = app::App::new(data, recordings);
    let _ = state.desktop_url.set(args.desktop_url.clone());
    match dimos_app::get().and_then(|app| app.name.clone()) {
        Some(name) if !args.desktop_url.is_empty() => desktop::spawn_relay(state.clone(), args.desktop_url.clone(), name),
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
        let socket = args.socket.clone().context("--socket or --port is required")?;
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
        axum::serve(listener, router).with_graceful_shutdown(shutdown).await?;
    }
    Ok(())
}
