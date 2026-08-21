// Copyright (c) 2023 Dark Energy Processor
//
// This software is provided 'as-is', without any express or implied
// warranty. In no event will the authors be held liable for any damages
// arising from the use of this software.
//
// Permission is granted to anyone to use this software for any purpose,
// including commercial applications, and to alter it and redistribute it
// freely, subject to the following restrictions:
//
// 1. The origin of this software must not be misrepresented; you must not
//    claim that you wrote the original software. If you use this software
//    in a product, an acknowledgment in the product documentation would be
//    appreciated but is not required.
// 2. Altered source versions must be plainly marked as such, and must not be
//    misrepresented as being the original software.
// 3. This notice may not be removed or altered from any source distribution.

use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Args;
use tower_http::{
    catch_panic::CatchPanicLayer, cors::CorsLayer, services::ServeDir, trace::TraceLayer,
};
use tracing::info;

use crate::config::Config;
use crate::file_handler::{sanitize_path, FileState};
use crate::models::*;

// ── Program version constants ──────────────────────────────────────────────────

const DLAPI_MAJOR_VERSION: u32 = 1;
const DLAPI_MINOR_VERSION: u32 = 1;
const NPPS4_DLAPI_PROGRAM_VERSION: (u32, u32, u32) = (2023, 5, 14);

// ── Application state ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    files: Arc<FileState>,
    git_commit: Arc<String>,
}

// ── Access control middleware ──────────────────────────────────────────────────

async fn verify_api_access(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let shared_key = request
        .headers()
        .get("dlapi-shared-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if !state.config.is_accessible(&path, shared_key.as_deref()) {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponseModel { detail: "Not found.".into() }),
        )
            .into_response();
    }

    next.run(request).await
}

// ── URL builder ────────────────────────────────────────────────────────────────

/// Build a full URL for a path under /archive-root/.
/// If `base_url` is configured in config, that takes precedence over the
/// incoming Host header (prevents Host header injection / cache poisoning).
fn archive_url(headers: &HeaderMap, base_url: Option<&str>, path: &str) -> String {
    let clean = path.trim_start_matches('/');
    if let Some(base) = base_url {
        return format!("{base}/archive-root/{clean}");
    }
    // Fallback: derive from request headers (only safe when not behind a cache)
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:8000");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}/archive-root/{clean}")
}

// ── Helper response builders ───────────────────────────────────────────────────

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponseModel { detail: msg.into() }),
    )
        .into_response()
}

/// Run blocking filesystem work off the async runtime.
///
/// All `FileState` methods do synchronous file IO (and may parse very large
/// JSON files); running them inline would stall tokio's worker threads and
/// make the whole server unresponsive under load. This also isolates panics:
/// a panicking task yields an error here instead of aborting the connection.
async fn run_blocking<T, F>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    // block_in_place demotes this worker thread for the duration instead of
    // dispatching to the blocking pool: same protection against stalling the
    // runtime, without a per-request task hop. Panics unwind into
    // CatchPanicLayer, which still turns them into a 500 response.
    tokio::task::block_in_place(f)
}

fn database_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponseModel { detail: "Database not found".into() }),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponseModel { detail: "Internal server error".into() }),
    )
        .into_response()
}

/// Map platform integer (1=iOS, 2=Android) to zero-based index.
fn validate_platform(platform: u8) -> Option<usize> {
    match platform {
        1 => Some(0), // iOS
        2 => Some(1), // Android
        _ => None,
    }
}

// ── Route handlers ─────────────────────────────────────────────────────────────

/// GET /api/publicinfo
async fn publicinfo_handler(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> impl IntoResponse {
    let files = state.files.clone();
    let (major, minor) = match run_blocking(move || files.get_latest_version()).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("get_latest_version: {e}");
            return internal_error();
        }
    };

    let mut application = HashMap::new();
    application.insert("NPPS4DLAPICommit".into(), state.git_commit.as_ref().clone());
    application.insert(
        "NPPS4DLAPIVersion".into(),
        format!(
            "{}.{:02}.{:02}",
            NPPS4_DLAPI_PROGRAM_VERSION.0,
            NPPS4_DLAPI_PROGRAM_VERSION.1,
            NPPS4_DLAPI_PROGRAM_VERSION.2
        ),
    );

    let info = PublicInfoModel {
        public_api: state.config.is_public_accessible(),
        dlapi_version: VersionModel {
            major: DLAPI_MAJOR_VERSION,
            minor: DLAPI_MINOR_VERSION,
        },
        serve_time_limit: 0,
        game_version: format!("{major}.{minor}"),
        application,
    };

    (StatusCode::OK, Json(info)).into_response()
}

/// POST /api/v1/update
async fn update_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(param): Json<UpdateRequest>,
) -> impl IntoResponse {
    let platform = match validate_platform(param.platform) {
        Some(p) => p,
        None => return bad_request("Invalid platform"),
    };

    let files = state.files.clone();
    let version = param.version.clone();
    match run_blocking(move || files.get_update_file(&version, platform)).await {
        Ok(mut downloads) => {
            for d in &mut downloads {
                d.url = archive_url(&headers, state.config.base_url.as_deref(), &d.url);
            }
            (StatusCode::OK, Json(downloads)).into_response()
        }
        Err(e) => {
            tracing::error!("get_update_file: {e}");
            internal_error()
        }
    }
}

/// POST /api/v1/batch
async fn batch_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(param): Json<BatchDownloadRequest>,
) -> impl IntoResponse {
    let platform = match validate_platform(param.platform) {
        Some(p) => p,
        None => return bad_request("Invalid platform"),
    };

    let files = state.files.clone();
    let (package_type, exclude) = (param.package_type, param.exclude);
    match run_blocking(move || files.get_batch_list(package_type, platform, &exclude)).await {
        Ok(Some(mut downloads)) => {
            for d in &mut downloads {
                d.url = archive_url(&headers, state.config.base_url.as_deref(), &d.url);
            }
            (StatusCode::OK, Json(downloads)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponseModel { detail: "Package type not found".into() }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("get_batch_list: {e}");
            internal_error()
        }
    }
}

/// POST /api/v1/download
async fn download_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(param): Json<DownloadRequest>,
) -> impl IntoResponse {
    let platform = match validate_platform(param.platform) {
        Some(p) => p,
        None => return bad_request("Invalid platform"),
    };

    let files = state.files.clone();
    let (package_type, package_id) = (param.package_type, param.package_id);
    match run_blocking(move || files.get_single_package(package_type, package_id, platform)).await {
        Ok(Some(mut downloads)) => {
            for d in &mut downloads {
                d.url = archive_url(&headers, state.config.base_url.as_deref(), &d.url);
            }
            (StatusCode::OK, Json(downloads)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponseModel { detail: "Package not found".into() }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("get_single_package: {e}");
            internal_error()
        }
    }
}

/// GET /api/v1/getdb/{name}
async fn getdb_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let files = state.files.clone();
    let lookup_name = name.clone();
    match run_blocking(move || files.get_database_path(&lookup_name)).await {
        Ok(Some((path, size))) => {
            // Stream from disk instead of buffering the whole database
            // (decrypted SIF databases can be tens of MB per request).
            let file = match tokio::fs::File::open(&path).await {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return database_not_found();
                }
                Err(e) => {
                    tracing::error!("open {}: {e}", path.display());
                    return internal_error();
                }
            };
            // Only ASCII survives here: header values must be valid HTTP, and
            // a non-ASCII filename would make the response builder fail.
            let db_name: String = name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/vnd.sqlite3".to_string()),
                    (header::CONTENT_LENGTH, size.to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{db_name}.db_\""),
                    ),
                ],
                // 256 KiB chunks: the 4 KiB default costs a poll+frame per page and
                // measurably hurts throughput on multi-MB databases.
                Body::from_stream(tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024)),
            )
                .into_response()
        }
        Ok(None) => database_not_found(),
        Err(e) => {
            tracing::error!("get_database_path: {e}");
            internal_error()
        }
    }
}

/// POST /api/v1/getfile
async fn getfile_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(param): Json<MicroDownloadRequest>,
) -> impl IntoResponse {
    let platform = match validate_platform(param.platform) {
        Some(p) => p,
        None => return bad_request("Invalid platform"),
    };

    let files = state.files.clone();
    let requested = param.files;
    let result = run_blocking(move || {
        let mut results = Vec::with_capacity(requested.len());
        for file_path in &requested {
            results.push(files.get_microdl_file(file_path, platform)?);
        }
        Ok(results)
    })
    .await;

    match result {
        Ok(mut results) => {
            for info in &mut results {
                info.url = archive_url(&headers, state.config.base_url.as_deref(), &info.url);
            }
            (StatusCode::OK, Json(results)).into_response()
        }
        Err(e) => {
            tracing::error!("get_microdl_file: {e}");
            internal_error()
        }
    }
}

/// GET /api/v1/release_info
async fn release_info_handler(State(state): State<AppState>) -> impl IntoResponse {
    let files = state.files.clone();
    match run_blocking(move || files.get_release_info()).await {
        Ok(map) => (StatusCode::OK, Json(map)).into_response(),
        Err(e) => {
            tracing::error!("get_release_info: {e}");
            internal_error()
        }
    }
}

// ── Health check ───────────────────────────────────────────────────────────────

/// GET /health
///
/// Liveness/readiness probe for reverse proxies and monitoring. Returns 200
/// when the archive is readable, 503 when the process is up but the archive
/// is broken — so operators can tell those two failure modes apart.
/// Intentionally outside the shared-key middleware: it exposes nothing but
/// the latest game version, which /api/publicinfo commonly exposes anyway.
async fn health_handler(State(state): State<AppState>) -> Response {
    let files = state.files.clone();
    match run_blocking(move || files.get_latest_version()).await {
        Ok((major, minor)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "gameVersion": format!("{major}.{minor}") })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "degraded", "detail": e.to_string() })),
        )
            .into_response(),
    }
}

// ── Graceful shutdown ──────────────────────────────────────────────────────────

/// Resolve when SIGTERM or Ctrl+C (SIGINT) is received, letting axum drain
/// in-flight requests instead of dropping connections mid-response (which
/// reverse proxies surface to users as 502 during every restart).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutdown signal received, draining in-flight requests...");
}

// ── Legacy /v7 micro-download route ───────────────────────────────────────────

/// GET /v7/micro_download/{platform}/{version}/{*file_path}
///
/// Reverse-maps the old v7 CDN path format to the current archive-root layout:
///   /v7/micro_download/{platform}/{version}/{file}
///   → {archive_root}/{Platform}/package/{version}/microdl/{file}
async fn v7_microdl_handler(
    State(state): State<AppState>,
    Path((platform, version, file_path)): Path<(String, String, String)>,
) -> Response {
    let platform_dir = match platform.to_lowercase().as_str() {
        "android" => "Android",
        "ios" => "iOS",
        _ => {
            return (StatusCode::NOT_FOUND, Json(ErrorResponseModel { detail: "Not found.".into() }))
                .into_response();
        }
    };

    // Sanitize version: only digits and dots
    let version_clean: String = version.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();

    let sanitized_file = sanitize_path(&file_path);
    if sanitized_file.is_empty() {
        return (StatusCode::NOT_FOUND, Json(ErrorResponseModel { detail: "Not found.".into() }))
            .into_response();
    }

    let fs_path = state.files.archive_root
        .join(platform_dir)
        .join("package")
        .join(&version_clean)
        .join("microdl")
        .join(&sanitized_file);

    // Stream from disk instead of buffering the whole asset in memory.
    let not_found = || {
        (StatusCode::NOT_FOUND, Json(ErrorResponseModel { detail: "Not found.".into() }))
            .into_response()
    };
    let file = match tokio::fs::File::open(&fs_path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return not_found(),
        Err(_) => return internal_error(),
    };
    let meta = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return internal_error(),
    };
    if !meta.is_file() {
        return not_found();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, meta.len().to_string()),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024)),
    )
        .into_response()
}

// ── Entry point ────────────────────────────────────────────────────────────────

/// Arguments for the `serve` subcommand (also the defaults when running
/// without a subcommand).
#[derive(Debug, Default, Args)]
pub struct ServeArgs {
    /// Enable permissive (wildcard) CORS on `/api/v1/getdb/{name}` so
    /// browser-based tools (e.g. sqlite-viewer) can fetch the game database
    /// cross-origin. Disabled by default.
    #[arg(long)]
    pub getdb_cors: bool,
}

pub async fn run(args: ServeArgs) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "n4dlapi=info,tower_http=info".parse().unwrap()),
        )
        .init();

    let config = Config::load()?;
    let archive_root = config.archive_root.clone();

    info!("Archive root: {}", archive_root.display());

    let git_commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    info!("Git commit: {git_commit}");
    info!(
        "DLAPI version: {DLAPI_MAJOR_VERSION}.{DLAPI_MINOR_VERSION}, program version: {}.{:02}.{:02}",
        NPPS4_DLAPI_PROGRAM_VERSION.0,
        NPPS4_DLAPI_PROGRAM_VERSION.1,
        NPPS4_DLAPI_PROGRAM_VERSION.2,
    );

    let state = AppState {
        config: Arc::new(config),
        files: Arc::new(FileState::new(archive_root.clone())),
        git_commit: Arc::new(git_commit),
    };

    // GET /api/v1/getdb/{name} — the wildcard CORS layer is only applied with
    // --getdb-cors; it lets external tools (e.g. sqlite-viewer) fetch the
    // database directly from the browser.
    let mut getdb_route = Router::new().route("/api/v1/getdb/{name}", get(getdb_handler));
    if args.getdb_cors {
        info!("Wildcard CORS enabled on /api/v1/getdb/* (--getdb-cors)");
        getdb_route = getdb_route.layer(CorsLayer::permissive());
    }
    let getdb_route =
        getdb_route.layer(middleware::from_fn_with_state(state.clone(), verify_api_access));

    // API routes with access control middleware
    let api_routes = Router::new()
        .route("/api/publicinfo", get(publicinfo_handler))
        .route("/api/v1/update", post(update_handler))
        .route("/api/v1/batch", post(batch_handler))
        .route("/api/v1/download", post(download_handler))
        .route("/api/v1/getfile", post(getfile_handler))
        .route("/api/v1/release_info", get(release_info_handler))
        .layer(middleware::from_fn_with_state(state.clone(), verify_api_access));

    // /v7/micro_download is kept for legacy compatibility.
    let static_routes = Router::new()
        .route("/v7/micro_download/{platform}/{version}/{*file_path}", get(v7_microdl_handler))
        .route("/health", get(health_handler));

    // Serve /archive-root/* directly, like the original Python implementation
    // mounts StaticFiles there. Download URLs returned by the API point here,
    // so a standalone deployment must serve these files itself. A reverse
    // proxy (nginx) MAY still intercept /archive-root for performance; this
    // route is simply never reached in that case.
    let app = Router::new()
        .merge(getdb_route)
        .merge(api_routes)
        .merge(static_routes)
        .nest_service("/archive-root", ServeDir::new(&archive_root))
        .layer(TraceLayer::new_for_http())
        // A panicking handler must produce a 500 response; otherwise the
        // connection is dropped and reverse proxies surface it as 502.
        .layer(CatchPanicLayer::new())
        // The original implementation imposes no request size limit; keep a
        // generous cap so large /api/v1/getfile batches aren't rejected.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);

    let listen_addr = std::env::var("N4DLAPI_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8000".to_string());
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    info!("Listening on http://{listen_addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("Shutdown complete.");
    Ok(())
}
