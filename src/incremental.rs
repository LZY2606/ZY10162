//! 增量重放：在旧运行快照上重算，标注哪些分位数被复用、哪些失效。
//!
//! 失效口径：
//! - 结构性变化（节点深度集合、边跨度、缺芯布局变化）→ 全部失效；
//! - 某个锦标变化（含暂时排除/恢复/分布调整）→ 仅 dep_markers 含它的节点/边失效；
//! - 某缺芯下界变化 → 仅 dep_gaps 含它的节点/边失效；
//! - 被复用的分位数必须与旧运行逐位一致（确定性重放的硬校验）。

use crate::model::*;
use crate::solver::{solve, EdgeQ, NodeQ, SolveResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ChangeKeys {
    pub markers: Vec<String>,
    pub gaps: Vec<String>,
    pub structural: bool,
    pub detail: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReusedNode {
    pub depth: f64,
    pub reused: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReusedEdge {
    pub depth_from: f64,
    pub depth_to: f64,
    pub reused: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RerunReport {
    pub result: SolveResult,
    pub node_status: Vec<ReusedNode>,
    pub edge_status: Vec<ReusedEdge>,
    pub changes: ChangeKeys,
    pub reused_node_count: usize,
    pub invalidated_node_count: usize,
}

fn signature_keys<T: serde::Serialize>(items: &[T]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|x| {
            let v = serde_json::to_value(x).unwrap();
            let id = v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            (id, v.to_string())
        })
        .collect()
}

/// 对比两次求解输入，得出变化的锦标 / 缺芯键与结构变化标记。
pub fn diff_inputs(base: &SolveInput, now: &SolveInput) -> ChangeKeys {
    let mut detail = Vec::new();
    let mut marker_keys: Vec<String> = Vec::new();

    let base_m = signature_keys(&base.markers);
    let now_m = signature_keys(&now.markers);
    let all_marker_ids: std::collections::BTreeSet<String> = base_m
        .iter()
        .chain(now_m.iter())
        .map(|(id, _)| id.clone())
        .collect();
    for id in &all_marker_ids {
        let b = base_m.iter().find(|(x, _)| x == id).map(|(_, s)| s.as_str());
        let n = now_m.iter().find(|(x, _)| x == id).map(|(_, s)| s.as_str());
        if b != n {
            marker_keys.push(id.clone());
            detail.push(format!("锦标 {} 已变更/排除/恢复", id));
        }
    }

    let mut gap_keys: Vec<String> = Vec::new();
    let seg_sig = |sigs: &[(String, String)], id: &str| {
        sigs.iter().find(|(x, _)| x == id).map(|(_, s)| s.clone())
    };
    let base_s = signature_keys(&base.segments);
    let now_s = signature_keys(&now.segments);
    let all_seg: std::collections::BTreeSet<String> = base_s
        .iter()
        .chain(now_s.iter())
        .map(|(id, _)| id.clone())
        .collect();
    let mut structural = base.segments.len() != now.segments.len()
        || base.draws != now.draws
        || base.seed != now.seed;
    for id in &all_seg {
        let b = seg_sig(&base_s, id);
        let n = seg_sig(&now_s, id);
        if b != n {
            // 仅 gap_min_years 数值变化 → 视为缺芯边界调整（键级失效）；
            // 深度边界/存在性变化 → 结构性变化。
            let bs = base.segments.iter().find(|s| &s.id == id);
            let ns = now.segments.iter().find(|s| &s.id == id);
            let only_gap_value = match (bs, ns) {
                (Some(bs), Some(ns)) => {
                    bs.top == ns.top
                        && bs.bottom == ns.bottom
                        && bs.present == ns.present
                        && bs.note == ns.note
                }
                _ => false,
            };
            if only_gap_value {
                if bs.map(|s| !s.present).unwrap_or(false)
                    || ns.map(|s| !s.present).unwrap_or(false)
                {
                    gap_keys.push(id.clone());
                    detail.push(format!("缺芯 {} 边界（最小年数）已调整", id));
                }
            } else {
                structural = true;
                detail.push(format!("芯段 {} 结构变化（深度/存在性）", id));
            }
        }
    }

    // 交换状态变化归到相关锦标键。
    for (b, n) in base.exchanges.iter().zip(now.exchanges.iter()) {
        if b != n {
            for m in &now.markers {
                if m.exchange_group.as_deref() == Some(n.group.as_str()) {
                    let key = m.id.clone();
                    if !marker_keys.contains(&key) {
                        marker_keys.push(key);
                    }
                }
            }
            detail.push(format!("交换组 {} 状态变化", n.group));
        }
    }

    marker_keys.sort();
    marker_keys.dedup();
    gap_keys.sort();
    gap_keys.dedup();
    ChangeKeys {
        markers: marker_keys,
        gaps: gap_keys,
        structural,
        detail,
    }
}

fn find_node<'a>(nodes: &'a [NodeQ], depth: f64) -> Option<&'a NodeQ> {
    nodes
        .iter()
        .find(|n| (n.depth - depth).abs() < 1e-9)
}

fn find_edge<'a>(edges: &'a [EdgeQ], from: f64, to: f64) -> Option<&'a EdgeQ> {
    edges
        .iter()
        .find(|e| (e.depth_from - from).abs() < 1e-9 && (e.depth_to - to).abs() < 1e-9)
}

/// 在新输入上完整重算，并逐节点/边判定复用。
pub fn rerun(base: &SolveInput, base_result: &SolveResult, now: &SolveInput) -> RerunReport {
    let changes = diff_inputs(base, now);
    let result = solve(now);

    let mut node_status = Vec::new();
    let mut reused_nodes = 0usize;

    if !result.feasible {
        return RerunReport {
            node_status: vec![],
            edge_status: vec![],
            changes,
            result,
            reused_node_count: 0,
            invalidated_node_count: 0,
        };
    }

    for node in &result.nodes {
        let mut reused = false;
        let reason: String;
        if changes.structural {
            reason = "结构变化：全量失效".into();
        } else {
            let hit_marker = changes
                .markers
                .iter()
                .any(|m| node.dep_markers.contains(m));
            let hit_gap = changes.gaps.iter().any(|g| node.dep_gaps.contains(g));
            match (hit_marker, hit_gap) {
                (false, false) => {
                    if let Some(old) = find_node(&base_result.nodes, node.depth) {
                        if old.q05 == node.q05 && old.median == node.median && old.q95 == node.q95 {
                            reused = true;
                            reason = "输入依赖未变化，分位数逐位一致".into();
                        } else {
                            reason = "依赖未变但数值漂移，保守失效".into();
                        }
                    } else {
                        reason = "旧运行无对应深度节点".into();
                    }
                }
                (true, _) => reason = "依赖的锦标已变更".into(),
                (_, true) => reason = "依赖的缺芯边界已调整".into(),
            }
        }
        if reused {
            reused_nodes += 1;
        }
        node_status.push(ReusedNode {
            depth: node.depth,
            reused,
            reason,
        });
    }

    let mut edge_status = Vec::new();
    for edge in &result.edges {
        let mut reused = false;
        let reason: String;
        if changes.structural {
            reason = "结构变化：全量失效".into();
        } else {
            let hit_marker = changes
                .markers
                .iter()
                .any(|m| edge.dep_markers.contains(m));
            let hit_gap = changes.gaps.iter().any(|g| edge.dep_gaps.contains(g));
            match (hit_marker, hit_gap) {
                (false, false) => {
                    if let Some(old) =
                        find_edge(&base_result.edges, edge.depth_from, edge.depth_to)
                    {
                        if old.rate_q05 == edge.rate_q05
                            && old.rate_median == edge.rate_median
                            && old.rate_q95 == edge.rate_q95
                        {
                            reused = true;
                            reason = "输入依赖未变化，速率分位数逐位一致".into();
                        } else {
                            reason = "依赖未变但数值漂移，保守失效".into();
                        }
                    } else {
                        reason = "旧运行无对应边".into();
                    }
                }
                (true, _) => reason = "依赖的锦标已变更".into(),
                (_, true) => reason = "依赖的缺芯边界已调整".into(),
            }
        }
        edge_status.push(ReusedEdge {
            depth_from: edge.depth_from,
            depth_to: edge.depth_to,
            reused,
            reason,
        });
    }

    let invalidated_nodes = node_status.len() - reused_nodes;
    RerunReport {
        result,
        node_status,
        edge_status,
        changes,
        reused_node_count: reused_nodes,
        invalidated_node_count: invalidated_nodes,
    }
}
