//! 运行记录导出 / 导入复核：
//! - 导出当前状态与全部不可变运行记录（JSON Bundle）；
//! - 清空数据库后导入，逐次用快照重放求解，并核对分位数与记录逐位一致。

use crate::db::{RunRecord, Store};
use crate::incremental::rerun;
use crate::model::*;
use crate::solver::solve;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub tool: String,
    pub solver_version: u32,
    pub segments: Vec<Segment>,
    pub markers: Vec<Marker>,
    pub exchanges: Vec<ExchangeState>,
    pub runs: Vec<RunRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyItem {
    pub seq: i64,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyReport {
    pub imported_runs: usize,
    pub items: Vec<VerifyItem>,
    pub all_ok: bool,
}

pub fn export(store: &Store) -> Result<Bundle, String> {
    let runs = store.list_runs().map_err(|e| e.to_string())?;
    Ok(Bundle {
        format: "han-ceng-nian-chi/bundle.v1".into(),
        tool: env!("CARGO_PKG_NAME").to_string(),
        solver_version: SOLVER_VERSION,
        segments: store.load_segments().map_err(|e| e.to_string())?,
        markers: store.load_markers().map_err(|e| e.to_string())?,
        exchanges: store.load_exchanges().map_err(|e| e.to_string())?,
        runs,
    })
}

/// 导入并重放复核。先清空，再重放每条记录；
/// feasible 记录的中位数/区间必须与 result_json 中逐位一致。
pub fn import_verify(store: &Store, bundle: Bundle) -> Result<VerifyReport, String> {
    if bundle.format != "han-ceng-nian-chi/bundle.v1" {
        return Err(format!("不支持的导出格式: {}", bundle.format));
    }
    if bundle.solver_version != SOLVER_VERSION {
        return Err(format!(
            "求解器版本不匹配：导出 v{}，当前 v{}",
            bundle.solver_version, SOLVER_VERSION
        ));
    }

    let mut items = Vec::new();
    // 先在内存里按快照顺序重放核对，再落库。
    let mut prev: Option<(SolveInput, crate::solver::SolveResult, i64)> = None;
    for rec in &bundle.runs {
        let fresh = solve(&rec.snapshot);
        if fresh.feasible != (rec.status == "feasible") {
            items.push(VerifyItem {
                seq: rec.seq,
                ok: false,
                detail: format!("可行性与记录不一致（记录 {}）", rec.status),
            });
            continue;
        }
        if !fresh.feasible {
            let stored: Option<crate::solver::ConflictSet> = rec
                .conflict_json
                .as_ref()
                .and_then(|j| serde_json::from_str(j).ok());
            let same = stored.as_ref() == fresh.conflict.as_ref();
            items.push(VerifyItem {
                seq: rec.seq,
                ok: same,
                detail: if same {
                    "最小冲突集与记录一致".into()
                } else {
                    "最小冲突集与记录不一致".into()
                },
            });
            continue;
        }
        let stored: crate::solver::SolveResult =
            serde_json::from_str(&rec.result_json).map_err(|e| e.to_string())?;
        let same = fresh
            .nodes
            .iter()
            .zip(stored.nodes.iter())
            .all(|(a, b)| a.depth == b.depth && a.q05 == b.q05 && a.median == b.median && a.q95 == b.q95)
            && fresh
                .edges
                .iter()
                .zip(stored.edges.iter())
                .all(|(a, b)| {
                    a.depth_from == b.depth_from
                        && a.depth_to == b.depth_to
                        && a.rate_median == b.rate_median
                });
        let mut detail = "分位数逐位一致".to_string();
        if let Some((base_in, base_res, base_seq)) = &prev {
            let report = rerun(base_in, base_res, &rec.snapshot);
            detail = format!(
                "{}；相对 #{} 复用 {} 节点 / 失效 {} 节点",
                detail,
                base_seq,
                report.reused_node_count,
                report.invalidated_node_count
            );
        }
        items.push(VerifyItem {
            seq: rec.seq,
            ok: same,
            detail: if same {
                detail
            } else {
                "分位数与记录不一致（重放不可复现）".into()
            },
        });
        prev = Some((rec.snapshot.clone(), fresh, rec.seq));
    }

    let all_ok = items.iter().all(|i| i.ok);
    if !all_ok {
        return Err("重放复核失败，已拒绝导入（数据库未改动）".into());
    }
    store.import_bundle(&bundle).map_err(|e| e.to_string())?;
    Ok(VerifyReport {
        imported_runs: bundle.runs.len(),
        items,
        all_ok: true,
    })
}
