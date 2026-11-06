//! HTTP layer: REST API (`/api/v1`), playlist output (`/p/<slug>.m3u`), OpenAPI and the embedded UI.

use std::sync::Arc;

use api_types::{
    Channel, ChannelCreate, ChannelPatch, EpgChannel, EpgFetchRequest, EpgFetchResult, EpgSource, EpgSourceCreate,
    EpgSourcePatch, ErrorBody, ErrorDetail, Hook, HookCreate, HookDelivery, HookPatch, ImportReport, ImportRequest,
    ItemAdd, ItemsReplace, Playlist, PlaylistCreate, PlaylistDetail, PlaylistPatch, RefreshRequest, RefreshResponse,
    ResolveRequest, ResolveResponse, Settings, SettingsPatch, SourceKind, Status,
};
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use utoipa::{IntoParams, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::epg::Epg;
use crate::store::{ChannelFilter, Store};
use crate::{Error, web};

/// Shared state of the HTTP layer. Derefs to the [`Store`].
#[derive(Clone)]
pub struct App {
    pub store: Arc<Store>,
    pub epg: Arc<Epg>,
    pub hooks: Arc<crate::hooks::Hooks>,
    /// Where temporary files (backups) are written; the data dir in production.
    scratch: std::path::PathBuf,
    /// Flips to true on shutdown, ending open event streams so the server can stop.
    shutdown: tokio::sync::watch::Receiver<bool>,
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
}

impl App {
    /// `data_dir` holds the EPG cache and backup scratch files.
    pub fn new(store: Arc<Store>, data_dir: &std::path::Path) -> Self {
        Self::with_timing(store, data_dir, crate::hooks::Timing::default())
    }

    /// As [`App::new`] with custom hook timing (tests). The hook dispatcher is not started; call
    /// `app.hooks.spawn()`.
    pub fn with_timing(store: Arc<Store>, data_dir: &std::path::Path, timing: crate::hooks::Timing) -> Self {
        let (shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        let epg = Epg::new(store.clone(), data_dir);
        App {
            hooks: crate::hooks::Hooks::new(store.clone(), epg.client().clone(), timing),
            epg,
            store,
            scratch: data_dir.to_path_buf(),
            shutdown,
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }

    /// End open event streams (call when the server starts shutting down).
    pub fn begin_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl std::ops::Deref for App {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

pub type AppState = App;

#[derive(OpenApi)]
#[openapi(
    info(title = "lineup", description = "Channel and playlist manager for Jellyfin Live TV"),
    components(schemas(SourceKind))
)]
struct ApiDoc;

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(status))
        .routes(routes!(list_channels, create_channel))
        .routes(routes!(get_channel, patch_channel, delete_channel))
        .routes(routes!(resolve))
        .routes(routes!(list_playlists, create_playlist))
        .routes(routes!(get_playlist, patch_playlist, delete_playlist))
        .routes(routes!(add_item, replace_items))
        .routes(routes!(remove_item))
        .routes(routes!(move_item))
        .routes(routes!(get_settings, patch_settings))
        .routes(routes!(import))
        .routes(routes!(refresh))
        .routes(routes!(list_epg_sources, create_epg_source))
        .routes(routes!(patch_epg_source, delete_epg_source))
        .routes(routes!(fetch_epg))
        .routes(routes!(search_epg_channels))
        .routes(routes!(list_hooks, create_hook))
        .routes(routes!(get_hook, patch_hook, delete_hook))
        .routes(routes!(test_hook))
        .routes(routes!(hook_deliveries))
}

/// The OpenAPI document (paths are relative to `/api/v1`).
pub fn openapi() -> utoipa::openapi::OpenApi {
    let (_, mut api) = api_router().split_for_parts();
    api.servers = Some(vec![utoipa::openapi::Server::new("/api/v1")]);
    api
}

/// Router over a fresh [`App`] whose files live in a new directory under the system temp dir (tests).
pub fn router(store: Arc<Store>) -> Router {
    let dir = std::env::temp_dir().join(format!(
        "lineup-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("creating a temp dir");
    router_for(App::new(store, &dir))
}

pub fn router_for(app: App) -> Router {
    let (api, _) = api_router().split_for_parts();
    // Not in OpenAPI: a binary file and an event stream.
    let api = api.route("/backup", get(backup)).route("/events", get(events));
    let doc = openapi();
    Router::new()
        .nest("/api/v1", api.fallback(api_not_found))
        .route("/api/openapi.json", get(move || async move { Json(doc) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/p/{file}", get(playlist_m3u))
        .route("/p/{slug}/epg.xml", get(playlist_epg))
        .fallback(web::static_handler)
        .with_state(app)
}

// -------------------------------------------------------------------- plumbing

pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!("{}", self.0);
        }
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.0.code().into(),
                message: self.0.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// `Json<T>` whose rejection is our JSON error body.
pub struct Body<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;
    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(e) => Err(ApiError(Error::Invalid(e.body_text()))),
        }
    }
}

/// `Path<T>` whose rejection is our JSON error body (code `invalid`).
pub struct ApiPath<T>(T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for ApiPath<T> {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(v)) => Ok(ApiPath(v)),
            Err(e) => Err(ApiError(Error::Invalid(e.body_text()))),
        }
    }
}

/// `Query<T>` whose rejection is our JSON error body (code `invalid`).
pub struct ApiQuery<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(v)) => Ok(ApiQuery(v)),
            Err(e) => Err(ApiError(Error::Invalid(e.body_text()))),
        }
    }
}

async fn api_not_found() -> ApiError {
    ApiError(Error::NotFound("no such API route".into()))
}

// -------------------------------------------------------------------- status

#[utoipa::path(get, path = "/status", responses((status = 200, body = Status)))]
async fn status() -> Json<Status> {
    Json(Status {
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

// -------------------------------------------------------------------- channels

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ChannelQuery {
    /// Exact group-title.
    group: Option<String>,
    /// Case-insensitive substring of name, tvg-id or source key.
    q: Option<String>,
    /// Only channels in this playlist.
    playlist: Option<String>,
    kind: Option<SourceKind>,
}

#[utoipa::path(get, path = "/channels", params(ChannelQuery), responses((status = 200, body = Vec<Channel>)))]
async fn list_channels(
    State(s): State<AppState>,
    ApiQuery(q): ApiQuery<ChannelQuery>,
) -> ApiResult<Json<Vec<Channel>>> {
    let filter = ChannelFilter {
        group: q.group,
        q: q.q,
        playlist: q.playlist,
        kind: q.kind,
    };
    Ok(Json(s.list_channels(&filter).await?))
}

#[utoipa::path(post, path = "/channels", request_body = ChannelCreate,
    responses((status = 201, body = Channel), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create_channel(
    State(s): State<AppState>,
    Body(req): Body<ChannelCreate>,
) -> ApiResult<(StatusCode, Json<Channel>)> {
    Ok((StatusCode::CREATED, Json(s.create_channel(req).await?)))
}

#[utoipa::path(get, path = "/channels/{id}", params(("id" = i64, Path)),
    responses((status = 200, body = Channel), (status = 404, body = ErrorBody)))]
async fn get_channel(State(s): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<Channel>> {
    Ok(Json(s.get_channel(id).await?))
}

#[utoipa::path(patch, path = "/channels/{id}", params(("id" = i64, Path)), request_body = ChannelPatch,
    responses((status = 200, body = Channel), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn patch_channel(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    Body(req): Body<ChannelPatch>,
) -> ApiResult<Json<Channel>> {
    Ok(Json(s.patch_channel(id, req).await?))
}

#[utoipa::path(delete, path = "/channels/{id}", params(("id" = i64, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_channel(State(s): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    s.delete_channel(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/resolve", request_body = ResolveRequest,
    responses((status = 200, body = ResolveResponse), (status = 422, body = ErrorBody)))]
async fn resolve(State(s): State<AppState>, Body(req): Body<ResolveRequest>) -> ApiResult<Json<ResolveResponse>> {
    Ok(Json(
        crate::resolve::resolve(&s.store, s.epg.client(), &req.input).await?,
    ))
}

// -------------------------------------------------------------------- playlists

#[utoipa::path(get, path = "/playlists", responses((status = 200, body = Vec<Playlist>)))]
async fn list_playlists(State(s): State<AppState>) -> ApiResult<Json<Vec<Playlist>>> {
    Ok(Json(s.list_playlists().await?))
}

#[utoipa::path(post, path = "/playlists", request_body = PlaylistCreate,
    responses((status = 201, body = PlaylistDetail), (status = 409, body = ErrorBody)))]
async fn create_playlist(
    State(s): State<AppState>,
    Body(req): Body<PlaylistCreate>,
) -> ApiResult<(StatusCode, Json<PlaylistDetail>)> {
    Ok((StatusCode::CREATED, Json(s.create_playlist(req).await?)))
}

#[utoipa::path(get, path = "/playlists/{slug}", params(("slug" = String, Path)),
    responses((status = 200, body = PlaylistDetail), (status = 404, body = ErrorBody)))]
async fn get_playlist(State(s): State<AppState>, ApiPath(slug): ApiPath<String>) -> ApiResult<Json<PlaylistDetail>> {
    Ok(Json(s.get_playlist(&slug).await?))
}

#[utoipa::path(patch, path = "/playlists/{slug}", params(("slug" = String, Path)), request_body = PlaylistPatch,
    responses((status = 200, body = PlaylistDetail), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn patch_playlist(
    State(s): State<AppState>,
    ApiPath(slug): ApiPath<String>,
    Body(req): Body<PlaylistPatch>,
) -> ApiResult<Json<PlaylistDetail>> {
    Ok(Json(s.patch_playlist(&slug, req).await?))
}

#[utoipa::path(delete, path = "/playlists/{slug}", params(("slug" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn delete_playlist(State(s): State<AppState>, ApiPath(slug): ApiPath<String>) -> ApiResult<StatusCode> {
    s.delete_playlist(&slug).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/playlists/{slug}/items", params(("slug" = String, Path)), request_body = ItemAdd,
    responses((status = 200, body = PlaylistDetail), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn add_item(
    State(s): State<AppState>,
    ApiPath(slug): ApiPath<String>,
    Body(req): Body<ItemAdd>,
) -> ApiResult<Json<PlaylistDetail>> {
    Ok(Json(s.add_item(&slug, req.channel_id, req.position).await?))
}

#[utoipa::path(put, path = "/playlists/{slug}/items", params(("slug" = String, Path)), request_body = ItemsReplace,
    responses((status = 200, body = PlaylistDetail), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn replace_items(
    State(s): State<AppState>,
    ApiPath(slug): ApiPath<String>,
    Body(req): Body<ItemsReplace>,
) -> ApiResult<Json<PlaylistDetail>> {
    Ok(Json(s.replace_items(&slug, req.channel_ids).await?))
}

#[utoipa::path(delete, path = "/playlists/{slug}/items/{channel_id}",
    params(("slug" = String, Path), ("channel_id" = i64, Path)),
    responses((status = 200, body = PlaylistDetail), (status = 404, body = ErrorBody)))]
async fn remove_item(
    State(s): State<AppState>,
    ApiPath((slug, channel_id)): ApiPath<(String, i64)>,
) -> ApiResult<Json<PlaylistDetail>> {
    Ok(Json(s.remove_item(&slug, channel_id).await?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MoveRequest {
    /// New 1-based position (clamped).
    position: i64,
}

#[utoipa::path(post, path = "/playlists/{slug}/items/{channel_id}/move",
    params(("slug" = String, Path), ("channel_id" = i64, Path)), request_body = MoveRequest,
    responses((status = 200, body = PlaylistDetail), (status = 404, body = ErrorBody)))]
async fn move_item(
    State(s): State<AppState>,
    ApiPath((slug, channel_id)): ApiPath<(String, i64)>,
    Body(req): Body<MoveRequest>,
) -> ApiResult<Json<PlaylistDetail>> {
    Ok(Json(s.move_item(&slug, channel_id, req.position).await?))
}

// -------------------------------------------------------------------- settings, import, refresh

#[utoipa::path(get, path = "/settings", responses((status = 200, body = Settings)))]
async fn get_settings(State(s): State<AppState>) -> ApiResult<Json<Settings>> {
    Ok(Json(s.settings().await?))
}

#[utoipa::path(patch, path = "/settings", request_body = SettingsPatch,
    responses((status = 200, body = Settings), (status = 422, body = ErrorBody)))]
async fn patch_settings(State(s): State<AppState>, Body(req): Body<SettingsPatch>) -> ApiResult<Json<Settings>> {
    Ok(Json(s.patch_settings(req).await?))
}

#[utoipa::path(post, path = "/import", request_body = ImportRequest,
    responses((status = 200, body = ImportReport), (status = 422, body = ErrorBody)))]
async fn import(State(s): State<AppState>, Body(req): Body<ImportRequest>) -> ApiResult<Json<ImportReport>> {
    let text = match (req.m3u_text, req.url) {
        (Some(t), None) => t,
        (None, Some(url)) => fetch_text(&url).await?,
        _ => return Err(Error::Invalid("exactly one of `m3u_text` or `url` is required".into()).into()),
    };
    Ok(Json(s.import(&text, &req.playlist_slug, req.dry_run).await?))
}

/// Largest m3u accepted by import-by-URL (direct imports are bounded by the request body limit).
pub const IMPORT_MAX_BYTES: usize = 2 * 1024 * 1024;

async fn fetch_text(url: &str) -> Result<String, Error> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| Error::Internal(e.to_string()))?;
    let mut res = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Invalid(format!("fetching {url}: {e}")))?;
    if !res.status().is_success() {
        return Err(Error::Invalid(format!("fetching {url}: HTTP {}", res.status())));
    }
    let too_big = || Error::Invalid(format!("fetching {url}: larger than {IMPORT_MAX_BYTES} bytes"));
    if res.content_length().is_some_and(|n| n > IMPORT_MAX_BYTES as u64) {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| Error::Invalid(format!("reading {url}: {e}")))?
    {
        if body.len() + chunk.len() > IMPORT_MAX_BYTES {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| Error::Invalid(format!("{url}: response is not UTF-8 text")))
}

#[utoipa::path(post, path = "/refresh", request_body = RefreshRequest,
    responses((status = 200, body = RefreshResponse), (status = 404, body = ErrorBody)))]
async fn refresh(State(s): State<AppState>, Body(req): Body<RefreshRequest>) -> ApiResult<Json<RefreshResponse>> {
    Ok(Json(RefreshResponse {
        slugs: s.refresh(req.slugs).await?,
    }))
}

// -------------------------------------------------------------------- EPG

#[utoipa::path(get, path = "/epg/sources", responses((status = 200, body = Vec<EpgSource>)))]
async fn list_epg_sources(State(s): State<AppState>) -> ApiResult<Json<Vec<EpgSource>>> {
    Ok(Json(s.list_epg_sources().await?))
}

/// Adds a source and starts fetching it in the background.
#[utoipa::path(post, path = "/epg/sources", request_body = EpgSourceCreate,
    responses((status = 201, body = EpgSource), (status = 422, body = ErrorBody)))]
async fn create_epg_source(
    State(s): State<AppState>,
    Body(req): Body<EpgSourceCreate>,
) -> ApiResult<(StatusCode, Json<EpgSource>)> {
    let src = s.create_epg_source(req).await?;
    if src.enabled {
        s.epg.spawn_fetch(src.id);
    }
    Ok((StatusCode::CREATED, Json(src)))
}

/// A URL change keeps the current guide data until the new URL's first successful fetch, which starts
/// immediately.
#[utoipa::path(patch, path = "/epg/sources/{id}", params(("id" = i64, Path)), request_body = EpgSourcePatch,
    responses((status = 200, body = EpgSource), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn patch_epg_source(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    Body(req): Body<EpgSourcePatch>,
) -> ApiResult<Json<EpgSource>> {
    let p = s.patch_epg_source(id, req).await?;
    if p.source.enabled && (p.url_changed || p.source.current_gen.is_none()) {
        s.epg.spawn_fetch(id);
    }
    Ok(Json(p.source))
}

#[utoipa::path(delete, path = "/epg/sources/{id}", params(("id" = i64, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_epg_source(State(s): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    s.delete_epg_source(id).await?;
    s.epg.spawn_forget(id);
    Ok(StatusCode::NO_CONTENT)
}

/// Fetch sources now and wait for the result. Fetches run one at a time, so this waits for any fetch
/// already running.
#[utoipa::path(post, path = "/epg/fetch", request_body = EpgFetchRequest,
    responses((status = 200, body = Vec<EpgFetchResult>), (status = 404, body = ErrorBody)))]
async fn fetch_epg(
    State(s): State<AppState>,
    Body(req): Body<EpgFetchRequest>,
) -> ApiResult<Json<Vec<EpgFetchResult>>> {
    Ok(Json(s.epg.fetch(&req.source_ids).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct EpgChannelQuery {
    /// Search text: matched against channel ids and display names.
    q: Option<String>,
    /// Maximum results (default 50).
    limit: Option<usize>,
}

/// Channels offered by the enabled sources. With `q`, the best matches first.
#[utoipa::path(get, path = "/epg/channels", params(EpgChannelQuery), responses((status = 200, body = Vec<EpgChannel>)))]
async fn search_epg_channels(
    State(s): State<AppState>,
    ApiQuery(q): ApiQuery<EpgChannelQuery>,
) -> ApiResult<Json<Vec<EpgChannel>>> {
    let all = s.epg_channels().await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(crate::suggest::search(all, q.q.as_deref(), limit)))
}

// -------------------------------------------------------------------- hooks

#[utoipa::path(get, path = "/hooks", responses((status = 200, body = Vec<Hook>)))]
async fn list_hooks(State(s): State<AppState>) -> ApiResult<Json<Vec<Hook>>> {
    Ok(Json(s.list_hooks().await?))
}

#[utoipa::path(post, path = "/hooks", request_body = HookCreate,
    responses((status = 201, body = Hook), (status = 422, body = ErrorBody)))]
async fn create_hook(State(s): State<AppState>, Body(req): Body<HookCreate>) -> ApiResult<(StatusCode, Json<Hook>)> {
    Ok((StatusCode::CREATED, Json(s.create_hook(req).await?)))
}

#[utoipa::path(get, path = "/hooks/{id}", params(("id" = i64, Path)),
    responses((status = 200, body = Hook), (status = 404, body = ErrorBody)))]
async fn get_hook(State(s): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<Hook>> {
    Ok(Json(s.get_hook(id).await?))
}

#[utoipa::path(patch, path = "/hooks/{id}", params(("id" = i64, Path)), request_body = HookPatch,
    responses((status = 200, body = Hook), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn patch_hook(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    Body(req): Body<HookPatch>,
) -> ApiResult<Json<Hook>> {
    Ok(Json(s.patch_hook(id, req).await?))
}

#[utoipa::path(delete, path = "/hooks/{id}", params(("id" = i64, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_hook(State(s): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    s.delete_hook(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Send a synthetic `hook.test` event now (one attempt, no debounce) and return the logged result.
/// For a Jellyfin hook this really starts a guide refresh.
#[utoipa::path(post, path = "/hooks/{id}/test", params(("id" = i64, Path)),
    responses((status = 200, body = HookDelivery), (status = 404, body = ErrorBody)))]
async fn test_hook(State(s): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<HookDelivery>> {
    Ok(Json(s.hooks.test(id).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct DeliveryQuery {
    /// Maximum rows (default 50).
    limit: Option<i64>,
}

/// Recent delivery attempts, newest first.
#[utoipa::path(get, path = "/hooks/{id}/deliveries", params(("id" = i64, Path), DeliveryQuery),
    responses((status = 200, body = Vec<HookDelivery>), (status = 404, body = ErrorBody)))]
async fn hook_deliveries(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeliveryQuery>,
) -> ApiResult<Json<Vec<HookDelivery>>> {
    let limit = q.limit.unwrap_or(50).clamp(1, crate::store::DELIVERIES_KEPT);
    Ok(Json(s.hook_deliveries(id, limit).await?))
}

// -------------------------------------------------------------------- playlist output

async fn playlist_m3u(State(s): State<AppState>, Path(file): Path<String>, headers: HeaderMap) -> Response {
    let Some(slug) = file.strip_suffix(".m3u8").or_else(|| file.strip_suffix(".m3u")) else {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    };
    let url_tvg = public_base(&headers).map(|b| format!("{b}/p/{slug}/epg.xml"));
    let body = match s.render_m3u_with(slug, url_tvg.as_deref()).await {
        Ok(b) => b,
        Err(Error::NotFound(m)) => return (StatusCode::NOT_FOUND, format!("{m}\n")).into_response(),
        Err(e) => return ApiError(e).into_response(),
    };
    let etag = format!("\"{:016x}\"", fnv1a64(body.as_bytes()));
    conditional(&headers, etag, "audio/x-mpegurl; charset=utf-8", body.into())
}

async fn playlist_epg(State(s): State<AppState>, Path(slug): Path<String>, headers: HeaderMap) -> Response {
    let r = match s.epg.playlist_xml(&slug).await {
        Ok(r) => r,
        Err(Error::NotFound(m)) => return (StatusCode::NOT_FOUND, format!("{m}\n")).into_response(),
        Err(e) => return ApiError(e).into_response(),
    };
    conditional(
        &headers,
        r.etag.clone(),
        "application/xml; charset=utf-8",
        r.body.clone(),
    )
}

/// `http(s)://host[:port]` as the client addressed us, for absolute URLs in outputs.
fn public_base(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host.is_empty() || host.contains(['/', ' ', '"']) {
        return None;
    }
    let proto = match headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) {
        Some("https") => "https",
        _ => "http",
    };
    Some(format!("{proto}://{host}"))
}

/// A body with an ETag, answering `If-None-Match` with 304.
fn conditional(headers: &HeaderMap, etag: String, content_type: &'static str, body: axum::body::Bytes) -> Response {
    let etag_hv = HeaderValue::from_str(&etag).expect("etag is ascii");
    let mut res = if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, content_type)], body).into_response()
    };
    res.headers_mut().insert(header::ETAG, etag_hv);
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

/// `GET /api/v1/events`: server-sent events. Each event's name is its type (`playlist.changed`,
/// `epg.updated`, ...) and its data the JSON payload. `ready` is sent on connect, and `resync` when
/// this client fell behind and missed events (refetch everything).
async fn events(State(app): State<AppState>) -> impl IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    let rx = app.store.subscribe();
    let shutdown = app.shutdown.clone();
    let ready = futures_util::stream::once(async {
        Ok::<_, std::convert::Infallible>(SseEvent::default().event("ready").data("{}"))
    });
    let updates = futures_util::stream::unfold((rx, shutdown), |(mut rx, mut shutdown)| async move {
        if *shutdown.borrow() {
            return None;
        }
        let ev = tokio::select! {
            r = rx.recv() => match r {
                Ok(e) => SseEvent::default().event(e.kind()).data(e.payload().to_string()),
                Err(RecvError::Lagged(_)) => SseEvent::default().event("resync").data("{}"),
                Err(RecvError::Closed) => return None,
            },
            _ = shutdown.changed() => return None,
        };
        Some((Ok(ev), (rx, shutdown)))
    });
    use futures_util::StreamExt;
    Sse::new(ready.chain(updates)).keep_alive(KeepAlive::default())
}

/// `GET /api/v1/backup`: a consistent SQLite snapshot (`VACUUM INTO`), streamed as a file.
async fn backup(State(app): State<AppState>) -> Response {
    let (store, scratch) = (&app.store, &app.scratch);
    let name = format!(
        ".lineup-backup-{}-{}.db",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let tmp = scratch.join(name);
    let result = async {
        store.backup_to(&tmp).await?;
        tokio::fs::read(&tmp)
            .await
            .map_err(|e| Error::Internal(format!("reading backup: {e}")))
    }
    .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    match result {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/vnd.sqlite3"),
                (header::CONTENT_DISPOSITION, "attachment; filename=\"lineup.db\""),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

pub(crate) fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
