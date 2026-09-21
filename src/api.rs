//! Axum 路由与处理器：操作页面所需的 JSON API。

use crate::db::{NewRun, RunRecord, Store};
use crate::exchange;
use crate::fixtures;
use crate::incremental::rerun;
use crate::model::*;
use crate::solver::solve;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
}

pub fn router(store: Store) -> Router {
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
    };
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/styles.css", get(styles))
        .route("/api/state", get(get_state))
        .route("/api/solve", post(post_solve))
        .route("/api/rerun", post(post_rerun))
        .route("/api/runs", get(list_runs))
        .route("/api/runs/{seq}", get(get_run))
        .route("/api/runs/{a}/diff/{b}", get(diff_runs))
        .route("/api/markers/{id}", post(patch_marker))
        .route("/api/segments/{id}", post(patch_segment))
        .route("/api/exchanges", post(post_exchange))
        .route("/api/admin/reset", post(admin_reset))
        .route("/api/admin/clear", post(admin_clear))
        .route("/api/export", get(do_export))
        .route("/api/import", post(do_import))
        .with_state(state)
}

async fn index() -> Response {
    serve_text(include_str!("../static/index.html"), "text/html; charset=utf-8")
}
async fn app_js() -> Response {
    serve_text(include_str!("../static/app.js"), "application/javascript; charset=utf-8")
}
async fn styles() -> Response {
    serve_text(include_str!("../static/styles.css"), "text/css; charset=utf-8")
}

fn serve_text(body: &'static str, content_type: &str) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(body))
        .unwrap()
}

fn api_error(code: StatusCode, msg: impl Into<String>) -> Response {
    let body = serde_json::json!({ "error": msg.into() }).to_string();
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .unwrap()
}

// ---------- 状态 ----------

#[derive(Serialize)]
struct StateResponse {
    segments: Vec<Segment>,
    markers: Vec<Marker>,
    exchanges: Vec<ExchangeState>,
    runs: Vec<RunSummary>,
}

#[derive(Serialize)]
struct RunSummary {
    seq: i64,
    kind: String,
    status: String,
    reason: String,
    seed: u64,
    draws: usize,
    parent_seq: Option<i64>,
}

impl From<&RunRecord> for RunSummary {
    fn from(r: &RunRecord) -> Self {
        RunSummary {
            seq: r.seq,
            kind: r.kind.clone(),
            status: r.status.clone(),
            reason: r.reason.clone(),
            seed: r.seed,
            draws: r.draws,
            parent_seq: r.parent_seq,
        }
    }
}

async fn get_state(State(st): State<AppState>) -> Response {
    let store = st.store.lock().unwrap();
    let segments = match store.load_segments() {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let markers = match store.load_markers() {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let exchanges = match store.load_exchanges() {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let runs = match store.list_runs() {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    Json(StateResponse {
        segments,
        markers,
        exchanges,
        runs: runs.iter().map(RunSummary::from).collect(),
    })
    .into_response()
}

#[derive(Deserialize, Default)]
struct SolveReq {
    seed: Option<u64>,
    draws: Option<usize>,
}

fn run_solve(store: &Store, seed: u64, draws: usize, kind: &str) -> Result<serde_json::Value, String> {
    let input = store.current_input(seed, draws).map_err(|e| e.to_string())?;
    let result = solve(&input);
    let feasible = result.feasible;
    let conflict_json = result
        .conflict
        .as_ref()
        .map(|c| serde_json::to_string(c).unwrap());
    let reason = result
        .conflict
        .as_ref()
        .map(|c| c.reason.clone())
        .unwrap_or_else(|| "可行模型".into());
    let seq = store.insert_run(&NewRun {
        kind,
        status: if feasible { "feasible" } else { "rejected" },
        reason,
        seed,
        draws,
        snapshot: &input,
        result_json: serde_json::to_string(&result).unwrap(),
        conflict_json,
        parent_seq: None,
        rerun_json: None,
    }).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "seq": seq, "result": result }))
}

async fn post_solve(State(st): State<AppState>, Json(req): Json<SolveReq>) -> Response {
    let store = st.store.lock().unwrap();
    let seed = req.seed.unwrap_or(fixtures::DEFAULT_SEED);
    let draws = req.draws.unwrap_or(fixtures::DEFAULT_DRAWS);
    if draws == 0 || draws > 100_000 {
        return api_error(StatusCode::BAD_REQUEST, "draws 必须在 1..=100000");
    }
    match run_solve(&store, seed, draws, "solve") {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct RerunReq {
    base_seq: i64,
}

async fn post_rerun(State(st): State<AppState>, Json(req): Json<RerunReq>) -> Response {
    let store = st.store.lock().unwrap();
    let base = match store.get_run(req.base_seq) {
        Ok(Some(v)) => v,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "基准运行不存在"),
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let base_result: crate::solver::SolveResult =
        match serde_json::from_str(&base.result_json) {
            Ok(v) => v,
            Err(_) => {
                return api_error(
                    StatusCode::CONFLICT,
                    "基准运行是被拒绝记录，无可重放分位数",
                )
            }
        };
    let now = match store.current_input(base.seed, base.draws) {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let report = rerun(&base.snapshot, &base_result, &now);
    let feasible = report.result.feasible;
    let conflict_json = report
        .result
        .conflict
        .as_ref()
        .map(|c| serde_json::to_string(c).unwrap());
    let reason = report
        .result
        .conflict
        .as_ref()
        .map(|c| c.reason.clone())
        .unwrap_or_else(|| {
            format!(
                "增量重放：复用 {} 节点，失效 {} 节点",
                report.reused_node_count, report.invalidated_node_count
            )
        });
    let rerun_json = serde_json::to_string(&report).ok();
    let seq = match store.insert_run(&NewRun {
        kind: "rerun",
        status: if feasible { "feasible" } else { "rejected" },
        reason,
        seed: base.seed,
        draws: base.draws,
        snapshot: &now,
        result_json: serde_json::to_string(&report.result).unwrap(),
        conflict_json,
        parent_seq: Some(base.seq),
        rerun_json,
    }) {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    Json(serde_json::json!({ "seq": seq, "report": report })).into_response()
}

async fn list_runs(State(st): State<AppState>) -> Response {
    let store = st.store.lock().unwrap();
    match store.list_runs() {
        Ok(v) => {
            let sum: Vec<RunSummary> = v.iter().map(RunSummary::from).collect();
            Json(sum).into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn get_run(State(st): State<AppState>, Path(seq): Path<i64>) -> Response {
    let store = st.store.lock().unwrap();
    match store.get_run(seq) {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "运行不存在"),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Serialize)]
struct NodeDiff {
    depth: f64,
    median_delta: f64,
    q05_delta: f64,
    q95_delta: f64,
}

#[derive(Serialize)]
struct EdgeDiff {
    depth_from: f64,
    depth_to: f64,
    rate_median_delta: f64,
    is_gap: bool,
}

#[derive(Serialize)]
struct RunDiff {
    a_seq: i64,
    b_seq: i64,
    nodes: Vec<NodeDiff>,
    edges: Vec<EdgeDiff>,
    max_abs_node_delta: f64,
    max_abs_edge_delta: f64,
}

async fn diff_runs(State(st): State<AppState>, Path((a, b)): Path<(i64, i64)>) -> Response {
    let store = st.store.lock().unwrap();
    let ra = match store.get_run(a) {
        Ok(Some(v)) => v,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "运行 A 不存在"),
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let rb = match store.get_run(b) {
        Ok(Some(v)) => v,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "运行 B 不存在"),
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let pa: crate::solver::SolveResult = match serde_json::from_str(&ra.result_json) {
        Ok(v) => v,
        Err(_) => return api_error(StatusCode::CONFLICT, "运行 A 无可行结果"),
    };
    let pb: crate::solver::SolveResult = match serde_json::from_str(&rb.result_json) {
        Ok(v) => v,
        Err(_) => return api_error(StatusCode::CONFLICT, "运行 B 无可行结果"),
    };
    let mut nodes = Vec::new();
    let mut max_node = 0.0f64;
    for nb in &pb.nodes {
        if let Some(na) = pa.nodes.iter().find(|x| (x.depth - nb.depth).abs() < 1e-9) {
            let d = NodeDiff {
                depth: nb.depth,
                median_delta: (nb.median - na.median).abs(),
                q05_delta: (nb.q05 - na.q05).abs(),
                q95_delta: (nb.q95 - na.q95).abs(),
            };
            max_node = max_node.max(d.median_delta);
            nodes.push(d);
        }
    }
    let mut edges = Vec::new();
    let mut max_edge = 0.0f64;
    for eb in &pb.edges {
        if let Some(ea) = pa
            .edges
            .iter()
            .find(|x| (x.depth_from - eb.depth_from).abs() < 1e-9)
        {
            let d = EdgeDiff {
                depth_from: eb.depth_from,
                depth_to: eb.depth_to,
                rate_median_delta: (eb.rate_median - ea.rate_median).abs(),
                is_gap: eb.is_gap,
            };
            max_edge = max_edge.max(d.rate_median_delta);
            edges.push(d);
        }
    }
    Json(RunDiff {
        a_seq: a,
        b_seq: b,
        nodes,
        edges,
        max_abs_node_delta: (max_node * 1e6).round() / 1e6,
        max_abs_edge_delta: (max_edge * 1e6).round() / 1e6,
    })
    .into_response()
}

// ---------- 编辑：锦标 / 芯段 / 交换 ----------

#[derive(Deserialize)]
struct MarkerPatch {
    depth: Option<f64>,
    kind: Option<String>,
    hard: Option<bool>,
    lo: Option<f64>,
    hi: Option<f64>,
    mean: Option<f64>,
    sd: Option<f64>,
    alt_means: Option<Vec<f64>>,
    exchange_group: Option<String>,
    weight: Option<f64>,
    excluded: Option<bool>,
    note: Option<String>,
    bump_version: Option<bool>,
}

async fn patch_marker(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<MarkerPatch>,
) -> Response {
    let store = st.store.lock().unwrap();
    let markers = match store.load_markers() {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let pos = match markers.iter().position(|m| m.id == id) {
        Some(p) => p,
        None => return api_error(StatusCode::NOT_FOUND, "锦标不存在"),
    };
    let mut m = markers[pos].clone();
    if let Some(v) = patch.depth {
        m.depth = v;
    }
    if let Some(v) = patch.kind {
        m.kind = v;
    }
    if let Some(v) = patch.hard {
        m.hard = v;
    }
    if let Some(v) = patch.lo {
        m.lo = v;
    }
    if let Some(v) = patch.hi {
        m.hi = v;
    }
    if let Some(v) = patch.mean {
        m.mean = Some(v);
    }
    if let Some(v) = patch.sd {
        m.sd = if v < 0.0 { None } else { Some(v) };
    }
    if let Some(v) = patch.alt_means {
        m.alt_means = v;
    }
    if let Some(v) = patch.exchange_group {
        m.exchange_group = if v.trim().is_empty() { None } else { Some(v) };
    }
    if let Some(v) = patch.weight {
        m.weight = v;
    }
    if let Some(v) = patch.excluded {
        m.excluded = v;
    }
    if let Some(v) = patch.note {
        m.note = v;
    }
    if patch.bump_version.unwrap_or(true) {
        m.version += 1;
    }
    if let Err(e) = m.validate() {
        return api_error(StatusCode::BAD_REQUEST, e.0);
    }
    // 深度严格有序：与其他锦标的深度仍需落在某个芯段内（构建问题时强校验），
    // 这里拒绝反向区间与负值深度。
    if m.depth < 0.0 {
        return api_error(StatusCode::BAD_REQUEST, "深度不能为负");
    }
    if m.hi < m.lo {
        return api_error(StatusCode::BAD_REQUEST, "区间 hi<lo");
    }
    if let Err(e) = store.upsert_marker(&m) {
        return api_error(StatusCode::BAD_REQUEST, e.to_string());
    }
    Json(m).into_response()
}

#[derive(Deserialize)]
struct SegmentPatch {
    top: Option<f64>,
    bottom: Option<f64>,
    present: Option<bool>,
    gap_min_years: Option<f64>,
    note: Option<String>,
    bump_version: Option<bool>,
}

async fn patch_segment(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<SegmentPatch>,
) -> Response {
    let store = st.store.lock().unwrap();
    let segments = match store.load_segments() {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let pos = match segments.iter().position(|s| s.id == id) {
        Some(p) => p,
        None => return api_error(StatusCode::NOT_FOUND, "芯段不存在"),
    };
    let mut s = segments[pos].clone();
    if let Some(v) = patch.top {
        s.top = v;
    }
    if let Some(v) = patch.bottom {
        s.bottom = v;
    }
    if let Some(v) = patch.present {
        s.present = v;
    }
    if let Some(v) = patch.gap_min_years {
        s.gap_min_years = Some(v);
    }
    if let Some(v) = patch.note {
        s.note = v;
    }
    if patch.bump_version.unwrap_or(true) {
        s.version += 1;
    }
    if let Err(e) = s.validate() {
        return api_error(StatusCode::BAD_REQUEST, e.0);
    }
    let mut all: Vec<Segment> = segments
        .iter()
        .filter(|x| x.id != id)
        .cloned()
        .collect();
    all.push(s.clone());
    if let Err(e) = validate_segments(&all) {
        return api_error(StatusCode::BAD_REQUEST, e.0);
    }
    if let Err(e) = store.upsert_segment(&s) {
        return api_error(StatusCode::BAD_REQUEST, e.to_string());
    }
    Json(s).into_response()
}

#[derive(Deserialize)]
struct ExchangeReq {
    group: String,
    swapped: bool,
}

async fn post_exchange(State(st): State<AppState>, Json(req): Json<ExchangeReq>) -> Response {
    let store = st.store.lock().unwrap();
    if let Err(e) = store.set_exchange(&req.group, req.swapped) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(ExchangeState {
        group: req.group,
        swapped: req.swapped,
    })
    .into_response()
}

async fn admin_reset(State(st): State<AppState>) -> Response {
    let store = st.store.lock().unwrap();
    if let Err(e) = store.reset_to_fixture() {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let v = match run_solve(&store, fixtures::DEFAULT_SEED, fixtures::DEFAULT_DRAWS, "baseline") {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    Json(serde_json::json!({ "reset": true, "baseline": v })).into_response()
}

async fn admin_clear(State(st): State<AppState>) -> Response {
    let store = st.store.lock().unwrap();
    if let Err(e) = store.clear_all() {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(serde_json::json!({ "cleared": true })).into_response()
}

async fn do_export(State(st): State<AppState>) -> Response {
    let store = st.store.lock().unwrap();
    match exchange::export(&store) {
        Ok(b) => Json(b).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn do_import(
    State(st): State<AppState>,
    Json(bundle): Json<exchange::Bundle>,
) -> Response {
    let store = st.store.lock().unwrap();
    match exchange::import_verify(&store, bundle) {
        Ok(report) => Json(report).into_response(),
        Err(e) => api_error(StatusCode::BAD_REQUEST, e),
    }
}
