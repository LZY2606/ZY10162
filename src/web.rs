//! Axum routes serving the local Web UI and JSON API.

use crate::db::Database;
use crate::model::{Evidence, Settings};
use crate::solver;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
}

pub struct AppError(axum::http::StatusCode, String);

fn err(status: axum::http::StatusCode, msg: impl Into<String>) -> AppError {
    AppError(status, msg.into())
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn db_error(error: rusqlite::Error) -> AppError {
    err(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
    )
}

fn solve_error(message: String) -> AppError {
    err(axum::http::StatusCode::UNPROCESSABLE_ENTITY, message)
}

pub fn router(db: Database) -> Router {
    let state = AppState {
        db: Arc::new(Mutex::new(db)),
    };
    Router::new()
        .route("/", get(index))
        .route("/api/workspace", get(workspace))
        .route("/api/evidence", post(upsert_evidence))
        .route("/api/evidence/{id}/enabled", post(set_enabled))
        .route("/api/evidence/{id}/gap", post(adjust_gap))
        .route("/api/soft/swap", post(swap_soft))
        .route("/api/evidence/{id}/distribution", post(adjust_distribution))
        .route("/api/settings", post(save_settings_route))
        .route("/api/solve", post(solve_now))
        .route("/api/runs", get(list_runs))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/runs/{id}/export", get(export_run))
        .route("/api/runs/compare", post(compare_runs))
        .route("/api/quantiles", get(get_quantiles))
        .route("/api/export", get(export_all))
        .route("/api/import", post(import_bundle))
        .route("/api/reset", post(reset_database))
        .with_state(state)
}

async fn index() -> Response {
    let html = include_str!("../static/index.html");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn workspace(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let evidence = db.list_evidence().map_err(db_error)?;
    let settings = db.load_settings();
    let cache = db.quantile_cache().map_err(db_error)?;
    Ok(Json(
        json!({ "evidence": evidence, "settings": settings, "quantile_cache": cache }),
    ))
}

async fn upsert_evidence(
    State(state): State<AppState>,
    Json(ev): Json<Evidence>,
) -> Result<Json<Value>, AppError> {
    if ev.id.trim().is_empty() {
        return Err(err(axum::http::StatusCode::BAD_REQUEST, "证据 id 不能为空"));
    }
    let db = state.db.lock().await;
    db.upsert_evidence(&ev).map_err(db_error)?;
    db.invalidate_all_quantiles().map_err(db_error)?;
    Ok(Json(json!({ "ok": true, "evidence": ev })))
}

#[derive(serde::Deserialize)]
struct EnabledBody {
    enabled: bool,
}

async fn set_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let changed = db.set_enabled(&id, body.enabled).map_err(db_error)?;
    if !changed {
        return Err(err(
            axum::http::StatusCode::NOT_FOUND,
            format!("证据 {} 不存在", id),
        ));
    }
    db.invalidate_all_quantiles().map_err(db_error)?;
    Ok(Json(
        json!({ "ok": true, "id": id, "enabled": body.enabled }),
    ))
}

#[derive(serde::Deserialize)]
struct GapBody {
    start: f64,
    end: f64,
    gap_mean: Option<f64>,
    gap_sigma: Option<f64>,
}

async fn adjust_gap(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<GapBody>,
) -> Result<Json<Value>, AppError> {
    if body.end <= body.start {
        return Err(err(
            axum::http::StatusCode::BAD_REQUEST,
            "缺芯结束深度必须严格大于开始深度",
        ));
    }
    let db = state.db.lock().await;
    let settings = db.load_settings();
    let evidence_before = db.list_evidence().map_err(db_error)?;
    let target = db.get_evidence(&id).map_err(db_error)?.ok_or_else(|| {
        err(
            axum::http::StatusCode::NOT_FOUND,
            format!("缺芯 {} 不存在", id),
        )
    })?;
    let stale =
        solver::stale_nodes_for_gap_move(&evidence_before, &settings, &id, body.start, body.end)
            .map_err(solve_error)?;
    let mut updated = target.clone();
    updated.depth = body.start;
    updated.gap_end = Some(body.end);
    if let Some(mean) = body.gap_mean {
        updated.gap_mean = Some(mean);
    }
    if let Some(sigma) = body.gap_sigma {
        updated.gap_sigma = Some(sigma);
    }
    db.upsert_evidence(&updated).map_err(db_error)?;
    db.invalidate_keys(&stale).map_err(db_error)?;
    Ok(Json(
        json!({ "ok": true, "evidence": updated, "invalidated_nodes": stale }),
    ))
}

#[derive(serde::Deserialize)]
struct DistributionBody {
    mean: f64,
    sigma: f64,
}

async fn adjust_distribution(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<DistributionBody>,
) -> Result<Json<Value>, AppError> {
    if body.sigma <= 0.0 {
        return Err(err(
            axum::http::StatusCode::BAD_REQUEST,
            "软锦标 σ 必须为正数",
        ));
    }
    let db = state.db.lock().await;
    let mut target = db.get_evidence(&id).map_err(db_error)?.ok_or_else(|| {
        err(
            axum::http::StatusCode::NOT_FOUND,
            format!("证据 {} 不存在", id),
        )
    })?;
    if !matches!(
        target.kind,
        crate::model::EvidenceKind::Ash | crate::model::EvidenceKind::Isotope
    ) {
        return Err(err(
            axum::http::StatusCode::CONFLICT,
            "仅火山灰/同位素软锦标可调整分布",
        ));
    }
    target.mean = Some(body.mean);
    target.sigma = Some(body.sigma);
    db.upsert_evidence(&target).map_err(db_error)?;
    db.invalidate_all_quantiles().map_err(db_error)?;
    Ok(Json(json!({ "ok": true, "evidence": target })))
}

async fn save_settings_route(
    State(state): State<AppState>,
    Json(settings): Json<Settings>,
) -> Result<Json<Value>, AppError> {
    if settings.draws == 0 || settings.quantiles.is_empty() {
        return Err(err(
            axum::http::StatusCode::BAD_REQUEST,
            "抽样数与分位数不能为空",
        ));
    }
    if settings.quantiles.iter().any(|p| !(0.0..=1.0).contains(p)) {
        return Err(err(
            axum::http::StatusCode::BAD_REQUEST,
            "分位数必须位于 [0,1]",
        ));
    }
    let db = state.db.lock().await;
    db.save_settings(&settings).map_err(db_error)?;
    Ok(Json(json!({ "ok": true, "settings": settings })))
}

async fn swap_soft(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let mut evidence = db.list_evidence().map_err(db_error)?;
    let soft_ids: Vec<String> = evidence
        .iter()
        .filter(|e| {
            (e.kind == crate::model::EvidenceKind::Ash
                || e.kind == crate::model::EvidenceKind::Isotope)
                && e.mean.is_some()
        })
        .map(|e| e.id.clone())
        .collect();
    if soft_ids.len() < 2 {
        return Err(err(
            axum::http::StatusCode::CONFLICT,
            "至少需要两个软锦标才能交换",
        ));
    }
    let id_a = soft_ids[0].clone();
    let id_b = soft_ids[1].clone();
    let pos_a = evidence.iter().position(|e| e.id == id_a).unwrap();
    let pos_b = evidence.iter().position(|e| e.id == id_b).unwrap();
    let dist_a = (evidence[pos_a].mean, evidence[pos_a].sigma);
    let dist_b = (evidence[pos_b].mean, evidence[pos_b].sigma);
    evidence[pos_a].mean = dist_b.0;
    evidence[pos_a].sigma = dist_b.1;
    evidence[pos_b].mean = dist_a.0;
    evidence[pos_b].sigma = dist_a.1;
    for ev in &evidence {
        if ev.id == id_a || ev.id == id_b {
            db.upsert_evidence(ev).map_err(db_error)?;
        }
    }
    db.invalidate_all_quantiles().map_err(db_error)?;
    Ok(Json(json!({ "ok": true, "swapped": [id_a, id_b] })))
}

async fn solve_now(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let evidence = db.list_evidence().map_err(db_error)?;
    let settings = db.load_settings();
    let output = solver::solve(&evidence, &settings).map_err(solve_error)?;
    db.store_run(&output.record).map_err(db_error)?;
    if output.feasible {
        db.replace_quantiles(&output.record).map_err(db_error)?;
    }
    Ok(Json(output.record))
}

async fn list_runs(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let runs = db.list_runs().map_err(db_error)?;
    Ok(Json(json!({ "runs": runs })))
}

async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let record = db.get_run(&id).map_err(db_error)?.ok_or_else(|| {
        err(
            axum::http::StatusCode::NOT_FOUND,
            format!("运行记录 {} 不存在", id),
        )
    })?;
    Ok(Json(record))
}

async fn export_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let db = state.db.lock().await;
    let record = db.get_run(&id).map_err(db_error)?.ok_or_else(|| {
        err(
            axum::http::StatusCode::NOT_FOUND,
            format!("运行记录 {} 不存在", id),
        )
    })?;
    let body = serde_json::to_vec_pretty(&record).unwrap();
    let disposition =
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"run-{}.json\"", id))
            .unwrap();
    Ok((
        [
            (
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

#[derive(serde::Deserialize)]
struct CompareBody {
    left: String,
    right: String,
    threshold: Option<f64>,
}

async fn compare_runs(
    State(state): State<AppState>,
    Json(body): Json<CompareBody>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let left = db.get_run(&body.left).map_err(db_error)?.ok_or_else(|| {
        err(
            axum::http::StatusCode::NOT_FOUND,
            format!("运行 {} 不存在", body.left),
        )
    })?;
    let right = db.get_run(&body.right).map_err(db_error)?.ok_or_else(|| {
        err(
            axum::http::StatusCode::NOT_FOUND,
            format!("运行 {} 不存在", body.right),
        )
    })?;
    let diff = compare_records(&left, &right, body.threshold.unwrap_or(1.0));
    Ok(Json(
        json!({ "left": body.left, "right": body.right, "diff": diff }),
    ))
}

pub fn compare_records(left: &Value, right: &Value, threshold: f64) -> Value {
    let index = |record: &Value| {
        record["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|node| (node["key"].as_str().unwrap_or("").to_string(), node))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let li = index(left);
    let ri = index(right);
    let mut segments = Vec::new();
    for (key, ln) in &li {
        if let Some(rn) = ri.get(key) {
            let lm = ln["median"].as_f64().unwrap_or(f64::NAN);
            let rm = rn["median"].as_f64().unwrap_or(f64::NAN);
            let delta = rm - lm;
            if delta.abs() >= threshold {
                segments.push(json!({
                    "node_key": key,
                    "depth": ln["depth"],
                    "left_median": lm,
                    "right_median": rm,
                    "delta_years": delta,
                }));
            }
        }
    }
    json!({ "threshold_years": threshold, "segments": segments })
}

async fn get_quantiles(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let cache = db.quantile_cache().map_err(db_error)?;
    Ok(Json(json!({ "quantiles": cache })))
}

async fn export_all(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let bundle = db.export_bundle().map_err(db_error)?;
    Ok(Json(bundle))
}

#[derive(serde::Deserialize)]
struct ImportBody {
    mode: Option<String>,
    bundle: Value,
}

async fn import_bundle(
    State(state): State<AppState>,
    Json(body): Json<ImportBody>,
) -> Result<Json<Value>, AppError> {
    let replace = body.mode.as_deref() != Some("merge");
    let db = state.db.lock().await;
    let report = db.import_bundle(&body.bundle, replace).map_err(db_error)?;
    Ok(Json(json!({
        "ok": true,
        "mode": if replace { "replace" } else { "merge" },
        "runs_added": report.runs_added,
        "runs_kept": report.runs_kept,
    })))
}

async fn reset_database(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let mut db = state.db.lock().await;
    db.clear().map_err(db_error)?;
    Database::load_fixture(&mut db).map_err(db_error)?;
    Ok(Json(json!({ "ok": true })))
}

#[allow(dead_code)]
fn settings_type_hint(_settings: &Settings) {}
