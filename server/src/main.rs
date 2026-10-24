//! Map Editor's `dimos-app-server` (dimOS Desktop app contract, docs/apps.md in dimos-desktop): serves the built
//! page and every action as an HTTP endpoint (api.rs), listed in /openapi.json for Desktop's agent. One compiled binary;
//! no Python or dimos at runtime.
mod api;
mod app;
mod desktop;
mod dimos_app;
mod icp_stitch;
mod persist;
mod pipeline;
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
    /// read recordings from this folder instead of asking the dimos gateway where they are (development, tests)
    #[arg(long)]
    recordings: Option<PathBuf>,
    /// print the endpoints (openapi.json) and exit: scripts/check_endpoints.ts compares them with dimos.yaml
    #[arg(long)]
    openapi_json: bool,
}

/// Where Desktop's shared recordings are. The dimos gateway is the authority: `GET /gateways/dimos/history/recordings`
/// answers `{ path }`. DIMOS_APP's `recordingsDir` is the answer when the gateway can't be reached, and a `recordings`
/// folder in the working directory is all that's left when the server runs on its own (`--port`, tests): nothing here
/// guesses at Desktop's own layout.
async fn recordings_dir(app: &dimos_app::DimosApp, desktop_url: &str) -> PathBuf {
    if !desktop_url.is_empty() {
        // startup waits on this, so keep it short: a gateway that doesn't answer falls back instead of holding up the page
        let asked = tokio::time::timeout(std::time::Duration::from_secs(5), desktop::get(desktop_url, "/gateways/dimos/history/recordings")).await;
        let fell_back = "using DIMOS_APP's recordingsDir";
        match asked {
            Ok(Ok(answer)) => match answer["path"].as_str().filter(|path| !path.is_empty()) {
                Some(path) => return PathBuf::from(path),
                None => eprintln!("map builder: the dimos gateway's history/recordings answered no path; {fell_back}"),
            },
            Ok(Err(error)) => eprintln!("map builder: the dimos gateway's history/recordings: {error:#}; {fell_back}"),
            Err(_) => eprintln!("map builder: the dimos gateway didn't answer history/recordings in time; {fell_back}"),
        }
    }
    app.recordings_dir.clone().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("recordings"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // DIMOS_APP is the whole interface (docs/apps.md in dimos-desktop); run outside Desktop, the defaults below
    let app = dimos_app::get().cloned().unwrap_or_default();
    let desktop_url = app.desktop_url.clone().unwrap_or_default();
    if args.openapi_json {
        println!("{}", serde_json::to_string_pretty(&api::openapi())?);
        return Ok(());
    }
    // Desktop's shared recordings folder (--recordings overrides it, as --port overrides DIMOS_APP's socket), and where
    // working sessions are autosaved: DIMOS_APP's dataDir, or a `data` folder in the working directory without it
    let recordings = match args.recordings.clone() {
        Some(folder) => folder,
        None => recordings_dir(&app, &desktop_url).await,
    };
    let data = app.data_dir.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("data"));
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
        // a recording dropped on the page streams in whole: no body limit there
        .merge(api::import_router().with_state(state.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A Desktop that answers one `GET /gateways/dimos/history/recordings` with `body`; returns its URL.
    async fn fake_desktop(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let read = stream.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            assert!(request.starts_with("GET /gateways/dimos/history/recordings HTTP/1.1"), "{request}");
            stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        url
    }

    fn app_with_recordings_dir() -> dimos_app::DimosApp {
        dimos_app::DimosApp { recordings_dir: Some("/from/dimos-app".into()), ..Default::default() }
    }

    #[tokio::test]
    async fn the_gateway_says_where_recordings_are() {
        let url = fake_desktop(r#"{"path":"/from/the/gateway"}"#).await;
        assert_eq!(recordings_dir(&app_with_recordings_dir(), &url).await, PathBuf::from("/from/the/gateway"));
    }

    #[tokio::test]
    async fn an_answer_without_a_path_is_no_answer() {
        let url = fake_desktop(r#"{"recordings":[]}"#).await;
        assert_eq!(recordings_dir(&app_with_recordings_dir(), &url).await, PathBuf::from("/from/dimos-app"));
    }

    #[tokio::test]
    async fn no_gateway_falls_back_to_dimos_app_and_then_to_the_working_directory() {
        // a port nothing listens on: the listener is bound to find a free one, then dropped
        let unreachable = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        assert_eq!(recordings_dir(&app_with_recordings_dir(), &unreachable).await, PathBuf::from("/from/dimos-app"));
        assert_eq!(recordings_dir(&app_with_recordings_dir(), "").await, PathBuf::from("/from/dimos-app"));
        // neither Desktop nor a gateway: nothing guesses at ~/.dimos
        assert_eq!(recordings_dir(&dimos_app::DimosApp::default(), "").await, PathBuf::from("recordings"));
    }
}
