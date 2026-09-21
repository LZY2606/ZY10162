//! 单调年代曲线求解器。
//!
//! 口径：
//! - 深度方向自上而下递增；年龄以“距表层年数”计，表层锚点为 0。
//! - 年龄随深度不减（非严格递增），相邻节点间允许零增长，缺芯边界为正增长。
//! - 硬区间是集合约束：同深度硬区间无交集即不可行；恰好相触（lo==hi）可行。
//! - 软锦标给出截断正态（或离散多方案）目标，在“单调锥 ∩ 硬盒约束”下
//!   用 Dykstra 交替投影（含加权 PAVA）求最小二乘解。
//! - 每次蒙特卡洛抽样使用按 (draw, 锦标 id) 派生的确定性子流，
//!   固定种子重放得到完全一致的分位数。

use crate::model::*;
use crate::rng::Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const EPS: f64 = 1e-9;
const DYKSTRA_ROUNDS: usize = 512;

#[derive(Clone, Debug)]
pub struct SNode {
    pub depth: f64,
    pub lo: f64,
    pub hi: f64,
    pub hard_markers: Vec<String>,
    pub soft_markers: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SEdge {
    /// 从节点 from 到 from+1 的最小年龄增量；缺芯边界 > 0
    pub glo: f64,
    /// 该边穿过的缺芯段 id（无则空）
    pub gap_of: Option<String>,
    pub depth_span: f64,
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub nodes: Vec<SNode>,
    pub edges: Vec<SEdge>,
    pub markers: Vec<Marker>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeQ {
    pub depth: f64,
    pub q05: f64,
    pub median: f64,
    pub q95: f64,
    pub dep_markers: Vec<String>,
    pub dep_gaps: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EdgeQ {
    pub depth_from: f64,
    pub depth_to: f64,
    pub rate_median: f64,
    pub rate_q05: f64,
    pub rate_q95: f64,
    pub is_gap: bool,
    pub gap_segment: Option<String>,
    pub dep_markers: Vec<String>,
    pub dep_gaps: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SolveResult {
    pub feasible: bool,
    pub nodes: Vec<NodeQ>,
    pub edges: Vec<EdgeQ>,
    pub conflict: Option<ConflictSet>,
    pub warnings: Vec<String>,
    pub draws: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConflictMarker {
    pub id: String,
    pub kind: String,
    pub depth: f64,
    pub lo: f64,
    pub hi: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConflictSet {
    pub reason: String,
    pub minimal: bool,
    pub markers: Vec<ConflictMarker>,
    pub gaps: Vec<String>,
}

/// 从快照构建有序节点与边。
pub fn build_problem(input: &SolveInput) -> Result<Problem, String> {
    validate_segments(&input.segments).map_err(|e| e.0)?;
    for m in &input.markers {
        m.validate().map_err(|e| e.0)?;
    }

    let mut depths: Vec<f64> = Vec::new();
    for s in &input.segments {
        depths.push(s.top);
        depths.push(s.bottom);
    }
    // 被排除锦标的深度也保留为自由插值节点，保证重放前后节点集合稳定。
    for m in &input.markers {
        depths.push(m.depth);
    }
    depths.sort_by(|a, b| a.partial_cmp(b).unwrap());
    depths.dedup_by(|a, b| (*a - *b).abs() <= EPS);

    let mut nodes: Vec<SNode> = depths
        .iter()
        .map(|&d| SNode {
            depth: d,
            lo: f64::NEG_INFINITY,
            hi: f64::INFINITY,
            hard_markers: Vec::new(),
            soft_markers: Vec::new(),
        })
        .collect();

    for m in input.markers.iter().filter(|m| !m.excluded) {
        let idx = nodes
            .iter()
            .position(|n| (n.depth - m.depth).abs() <= EPS)
            .expect("node depth");
        let node = &mut nodes[idx];
        if m.hard {
            node.lo = node.lo.max(m.lo);
            node.hi = node.hi.min(m.hi);
            node.hard_markers.push(m.id.clone());
        } else {
            node.soft_markers.push(m.id.clone());
        }
    }

    // 节点必须落在某个芯段的深度跨度内（含边界）。
    for node in &nodes {
        let inside = input
            .segments
            .iter()
            .any(|s| node.depth >= s.top - EPS && node.depth <= s.bottom + EPS);
        if !inside {
            return Err(format!("深度 {} 不在任何芯段范围内", node.depth));
        }
    }

    let mut edges: Vec<SEdge> = Vec::new();
    for w in nodes.windows(2) {
        let (d0, d1) = (w[0].depth, w[1].depth);
        let gap_seg = input.segments.iter().find(|s| {
            !s.present
                && d0 >= s.top - EPS
                && d0 < s.bottom - EPS
                && d1 > s.top + EPS
                && d1 <= s.bottom + EPS
        });
        let glo = match gap_seg {
            Some(s) => s.gap_min_years.unwrap(),
            None => 0.0,
        };
        edges.push(SEdge {
            glo,
            gap_of: gap_seg.map(|s| s.id.clone()),
            depth_span: d1 - d0,
        });
    }

    Ok(Problem {
        nodes,
        edges,
        markers: input.markers.clone(),
    })
}

#[derive(Clone)]
struct LB {
    /// 边 j -> i 的增量（j<i），或 kind "lo"/"hi" 的单点约束
    kind: &'static str,
    j: usize,
    w: f64,
    tag: String,
}

fn node_lbs(p: &Problem, i: usize) -> Vec<LB> {
    let mut v = vec![LB {
        kind: "lo",
        j: i,
        w: p.nodes[i].lo,
        tag: format!("lo:{}", i),
    }];
    if i > 0 {
        v.push(LB {
            kind: "edge",
            j: i - 1,
            w: p.edges[i - 1].glo,
            tag: format!("edge:{}", i - 1),
        });
    }
    v
}

/// 仅判断可行性：前向最小年龄 + 后向最大年龄的双包络扫描。
fn feasible_envelopes(p: &Problem) -> bool {
    let n = p.nodes.len();
    let mut lo = vec![f64::NEG_INFINITY; n];
    lo[0] = p.nodes[0].lo;
    for i in 1..n {
        let mut v = p.nodes[i].lo;
        v = v.max(lo[i - 1] + p.edges[i - 1].glo);
        lo[i] = v;
    }
    let mut hi = vec![f64::INFINITY; n];
    hi[n - 1] = p.nodes[n - 1].hi;
    for i in (0..n - 1).rev() {
        let mut v = p.nodes[i].hi;
        v = v.min(hi[i + 1] - p.edges[i].glo);
        hi[i] = v;
    }
    lo.iter().zip(hi.iter()).all(|(l, h)| *l <= *h + EPS)
}

/// 前向最长路（下界约束系统）。若某节点最小可达年龄超过其上界，
/// 沿前驱链回溯出参与的硬下界/边标签，并附上违例的 `hi:i`。
fn positive_cycle_tags(p: &Problem) -> Option<Vec<String>> {
    let n = p.nodes.len();
    let mut dist = vec![f64::NEG_INFINITY; n];
    let mut pred: Vec<Option<(usize, String)>> = vec![None; n];
    dist[0] = p.nodes[0].lo.max(0.0);
    for _ in 0..n {
        let mut changed = false;
        for i in 0..n {
            for lb in node_lbs(p, i) {
                let base = if lb.kind == "lo" { 0.0 } else { dist[lb.j] };
                if base != f64::NEG_INFINITY && base + lb.w > dist[i] + EPS {
                    dist[i] = base + lb.w;
                    pred[i] = Some((lb.j, lb.tag.clone()));
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for i in 0..n {
        if dist[i].is_finite() && dist[i] > p.nodes[i].hi + EPS {
            let mut tags = vec![format!("hi:{}", i)];
            let mut cur = i;
            for _ in 0..n {
                match &pred[cur] {
                    Some((pr, tag)) if pr != &cur => {
                        tags.push(tag.clone());
                        cur = *pr;
                    }
                    Some((_, tag)) => {
                        tags.push(tag.clone());
                        break;
                    }
                    None => break,
                }
            }
            tags.sort();
            tags.dedup();
            return Some(tags);
        }
    }
    None
}

fn marker_conflict(m: &Marker) -> ConflictMarker {
    ConflictMarker {
        id: m.id.clone(),
        kind: m.kind.clone(),
        depth: m.depth,
        lo: m.lo,
        hi: m.hi,
    }
}

/// 把 "lo:i"/"hi:i"/"edge:k" 标签解析为硬锦标集合与缺芯 id。
fn tags_to_markers(p: &Problem, tags: &[String]) -> (Vec<ConflictMarker>, Vec<String>) {
    let mut marker_ids: BTreeMap<String, ()> = BTreeMap::new();
    let mut gaps: BTreeMap<String, ()> = BTreeMap::new();
    for tag in tags {
        let (kind, idx) = tag.split_once(':').unwrap();
        let idx: usize = idx.parse().unwrap();
        match kind {
            "lo" | "hi" => {
                for m in &p.nodes[idx].hard_markers {
                    marker_ids.insert(m.clone(), ());
                }
            }
            "edge" => {
                if let Some(g) = &p.edges[idx].gap_of {
                    gaps.insert(g.clone(), ());
                }
            }
            _ => {}
        }
    }
    let markers = marker_ids
        .keys()
        .map(|id| marker_conflict(p.markers.iter().find(|m| &m.id == id).unwrap()))
        .collect();
    (markers, gaps.into_keys().collect())
}

/// 检查可行性，不可行时构造最小冲突集（对单锦标逐一删除验证极小性）。
pub fn check_feasible(p: &Problem) -> Option<ConflictSet> {
    // 1) 同深度硬区间无交集 —— 两两配对本身就是极小冲突集。
    for node in &p.nodes {
        let ids = &node.hard_markers;
        for a in 0..ids.len() {
            for b in (a + 1)..ids.len() {
                let ma = p.markers.iter().find(|m| m.id == ids[a]).unwrap();
                let mb = p.markers.iter().find(|m| m.id == ids[b]).unwrap();
                if ma.lo > mb.hi + EPS || mb.lo > ma.hi + EPS {
                    return Some(ConflictSet {
                        reason: format!(
                            "深度 {} 处硬约束区间无交集：{}[{},{}] 与 {}[{},{}]",
                            node.depth, ma.id, ma.lo, ma.hi, mb.id, mb.lo, mb.hi
                        ),
                        minimal: true,
                        markers: vec![marker_conflict(ma), marker_conflict(mb)],
                        gaps: vec![],
                    });
                }
            }
        }
    }

    // 2) 双包络快速判断；可行即放行（相触 lo==hi 在此通过）。
    if feasible_envelopes(p) {
        return None;
    }

    // 3) 正环回溯得到候选集，再逐一删除验证极小性。
    let tags = positive_cycle_tags(p).unwrap_or_default();
    let (cand_markers, cand_gaps) = tags_to_markers(p, &tags);
    let cand_ids: Vec<String> = cand_markers.iter().map(|m| m.id.clone()).collect();
    let mut minimal_ids: Vec<String> = cand_ids.clone();
    let mut minimal_gaps: Vec<String> = cand_gaps.clone();
    for id in &cand_ids {
        let trial: Vec<String> = minimal_ids.iter().filter(|x| *x != id).cloned().collect();
        if trial_conflict(p, &trial, &minimal_gaps) {
            minimal_ids = trial;
        }
    }
    for g in &cand_gaps {
        let trial_gaps: Vec<String> = minimal_gaps.iter().filter(|x| *x != g).cloned().collect();
        if trial_conflict(p, &minimal_ids, &trial_gaps) {
            minimal_gaps = trial_gaps;
        }
    }

    let markers: Vec<ConflictMarker> = minimal_ids
        .iter()
        .map(|id| {
            marker_conflict(p.markers.iter().find(|m| &m.id == id).unwrap())
        })
        .collect();

    Some(ConflictSet {
        reason: format!(
            "硬约束与单调/缺芯下界不一致：锦标 {} 与缺芯 {:?} 构成最小冲突集",
            minimal_ids.join(","),
            minimal_gaps
        ),
        minimal: true,
        markers,
        gaps: minimal_gaps,
    })
}

fn trial_conflict(p: &Problem, drop_markers: &[String], drop_gaps: &[String]) -> bool {
    let mut q = p.clone();
    q.markers.retain(|m| !drop_markers.contains(&m.id));
    for node in q.nodes.iter_mut() {
        node.hard_markers.retain(|id| !drop_markers.contains(id));
        node.lo = f64::NEG_INFINITY;
        node.hi = f64::INFINITY;
    }
    for edge in q.edges.iter_mut() {
        if let Some(g) = &edge.gap_of {
            if drop_gaps.contains(g) {
                edge.glo = 0.0;
            }
        }
    }
    for m in q.markers.iter().filter(|m| m.hard) {
        if let Some(node) = q.nodes.iter_mut().find(|n| (n.depth - m.depth).abs() <= EPS) {
            node.lo = node.lo.max(m.lo);
            node.hi = node.hi.min(m.hi);
        }
    }
    !feasible_envelopes(&q)
}

// ---------- 加权 PAVA（普通非减序，加权最小二乘投影） ----------

fn weighted_pava(values: &[f64], weights: &[f64]) -> Vec<f64> {
    struct Block {
        sum_w: f64,
        sum_wv: f64,
        start: usize,
        end: usize,
    }
    let n = values.len();
    let mut blocks: Vec<Block> = Vec::new();
    for i in 0..n {
        let mut cur = Block {
            sum_w: weights[i],
            sum_wv: weights[i] * values[i],
            start: i,
            end: i,
        };
        while let Some(top) = blocks.last() {
            let v_top = top.sum_wv / top.sum_w;
            let v_cur = cur.sum_wv / cur.sum_w;
            if v_top <= v_cur + EPS {
                break;
            }
            let t = blocks.pop().unwrap();
            cur = Block {
                sum_w: t.sum_w + cur.sum_w,
                sum_wv: t.sum_wv + cur.sum_wv,
                start: t.start,
                end: cur.end,
            };
        }
        blocks.push(cur);
    }
    let mut out = vec![0.0; n];
    for b in &blocks {
        let v = b.sum_wv / b.sum_w;
        for i in b.start..=b.end {
            out[i] = v;
        }
    }
    out
}

/// 盒约束 + 非减序（含缺芯偏移）的块不动点求解：
/// 每轮按当前块算加权均值并夹到公共盒区间；
/// - 夹到上/下边界 → 在边界节点处分裂块（Kuhn-Tucker 夹点）；
/// - 相邻块次序冲突 → 合并块；
/// 块划分稳定后即为带权最小二乘投影的精确解。
fn polish(
    z0: &[f64],
    targets: &[f64],
    weights: &[f64],
    hard_lo: &[f64],
    hard_hi: &[f64],
    glos: &[f64],
    groups0: &[usize],
) -> Vec<f64> {
    let n = targets.len();

    // 去掉缺芯偏移：令 u_i = z_i - G_i，则 glo 全部变成普通非减约束，
    // 但盒子变为 [lo_i-G_i, hi_i-G_i]。
    let g: Vec<f64> = std::iter::once(0.0)
        .chain(glos.iter().scan(0.0, |acc, x| {
            *acc += x;
            Some(*acc)
        }))
        .collect();
    let t: Vec<f64> = (0..n).map(|i| targets[i] - g[i]).collect();
    let lo: Vec<f64> = (0..n)
        .map(|i| if hard_lo[i].is_finite() { hard_lo[i] - g[i] } else { f64::NEG_INFINITY })
        .collect();
    let hi: Vec<f64> = (0..n)
        .map(|i| if hard_hi[i].is_finite() { hard_hi[i] - g[i] } else { f64::INFINITY })
        .collect();

    // 初始块取自 Dykstra 的 PAVA 分组（缺芯在变换空间仍可同值，不必强制分块）。
    let mut block_of: Vec<usize> = groups0.to_vec();
    let _ = z0;

    let block_value = |blocks: &[usize]| -> (Vec<(usize, usize, f64)>, Vec<usize>) {
        let mut out: Vec<(usize, usize, f64)> = Vec::new();
        let mut assign = vec![0usize; n];
        let mut l = 0usize;
        let mut bidx = 0usize;
        for i in 1..=n {
            if i == n || blocks[i] != blocks[l] {
                let r = i - 1;
                let sw: f64 = (l..=r).map(|k| weights[k]).sum();
                let mean = (l..=r).map(|k| weights[k] * t[k]).sum::<f64>() / sw;
                let blo = (l..=r).map(|k| lo[k]).filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
                let bhi = (l..=r).map(|k| hi[k]).filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
                let v = mean.clamp(blo, bhi);
                out.push((l, r, v));
                for k in l..=r {
                    assign[k] = bidx;
                }
                bidx += 1;
                l = i;
            }
        }
        (out, assign)
    };

    for _ in 0..256 {
        let (vals, assign) = block_value(&block_of);
        // 1) 逆序合并
        let mut merged = false;
        let mut new_blocks = block_of.clone();
        for w in vals.windows(2) {
            let (_, r1, v1) = w[0];
            let (l2, _, v2) = w[1];
            if v2 < v1 - 1e-9 {
                merged = true;
                let bg = assign[l2];
                for k in 0..n {
                    if assign[k] == bg {
                        new_blocks[k] = new_blocks[r1];
                    }
                }
            }
        }
        if merged {
            block_of = new_blocks;
            continue;
        }
        // 2) 夹边界分裂
        let mut split = false;
        for (l, r, v) in vals {
            if l == r {
                continue;
            }
            let blo = (l..=r).map(|k| lo[k]).filter(|x| x.is_finite()).fold(f64::NEG_INFINITY, f64::max);
            let bhi = (l..=r).map(|x| hi[x]).filter(|x| x.is_finite()).fold(f64::INFINITY, f64::min);
            let at_lo = (v - blo).abs() < 1e-8;
            let at_hi = (v - bhi).abs() < 1e-8;
            if at_lo || at_hi {
                split = true;
                // 找到造成边界的夹点节点，把块在夹点处切开。
                let pinch = if at_lo {
                    (l..=r).find(|&k| (lo[k] - blo).abs() < 1e-8).unwrap()
                } else {
                    (l..=r).find(|&k| (hi[k] - bhi).abs() < 1e-8).unwrap()
                };
                if pinch > l {
                    let id = 50_000 + pinch;
                    for k in pinch..=r {
                        block_of[k] = id;
                    }
                } else if pinch < r {
                    let id = 50_000 + pinch;
                    for k in l..=pinch {
                        block_of[k] = id;
                    }
                }
            }
        }
        if split {
            continue;
        }
        // 收敛：还原偏移并正向保证可行。
        let (vals, assign) = block_value(&block_of);
        let mut z = vec![0.0; n];
        for (bidx, (_, _, v)) in vals.iter().enumerate() {
            for k in 0..n {
                if assign[k] == bidx {
                    z[k] = v + g[k];
                }
            }
        }
        for i in 1..n {
            if z[i] < z[i - 1] + glos[i - 1] - 1e-8 {
                z[i] = z[i - 1] + glos[i - 1];
            }
        }
        return z;
    }
    z0.to_vec()
}

/// Dykstra 交替投影：在加权内积下，把软目标投影到
/// “非减锥 ∩ 硬盒 [lo,hi] ∩ 缺芯下界”。
/// 返回年龄解与 PAVA 最终池分组（用于依赖追踪）。
fn dykstra_solve(
    targets: &[f64],
    weights: &[f64],
    hard_lo: &[f64],
    hard_hi: &[f64],
    glos: &[f64],
) -> (Vec<f64>, Vec<usize>) {
    let n = targets.len();
    let off: Vec<f64> = std::iter::once(0.0)
        .chain(glos.iter().scan(0.0, |acc, g| {
            *acc += g;
            Some(*acc)
        }))
        .collect();

    let transform = |z: &[f64]| -> Vec<f64> {
        z.iter().enumerate().map(|(i, v)| v - off[i]).collect()
    };
    let untransform = |y: &[f64]| -> Vec<f64> {
        y.iter().enumerate().map(|(i, v)| v + off[i]).collect()
    };

    let boxed_lo: Vec<f64> = hard_lo
        .iter()
        .enumerate()
        .map(|(i, v)| if v.is_finite() { v - off[i] } else { f64::NEG_INFINITY })
        .collect();
    let boxed_hi: Vec<f64> = hard_hi
        .iter()
        .enumerate()
        .map(|(i, v)| if v.is_finite() { v - off[i] } else { f64::INFINITY })
        .collect();

    let mut y = transform(targets);
    let mut p = vec![0.0; n];
    let mut q = vec![0.0; n];

    for _ in 0..DYKSTRA_ROUNDS {
        // 投影到盒子：y + p
        let mut x = vec![0.0; n];
        for i in 0..n {
            let v = y[i] + p[i];
            x[i] = v.clamp(boxed_lo[i], boxed_hi[i]);
        }
        for i in 0..n {
            p[i] = y[i] + p[i] - x[i];
        }
        // 投影到非减锥：x + q（加权 PAVA）
        let arg: Vec<f64> = (0..n).map(|i| x[i] + q[i]).collect();
        let proj = weighted_pava(&arg, weights);
        for i in 0..n {
            q[i] = x[i] + q[i] - proj[i];
        }
        y = proj;
    }

    // 数值清理：迭代夹硬盒与下界，直到全部约束满足（Dykstra 256 轮后的
    // 残差只在 1e-7 量级，一次正反向传播即可收敛到可行点）。
    let mut z = untransform(&y);
    for _ in 0..32 {
        let mut moved = false;
        for i in 0..n {
            let c = z[i].clamp(hard_lo[i], hard_hi[i]);
            if c != z[i] { z[i] = c; moved = true; }
        }
        for i in 1..n {
            let need = z[i - 1] + glos[i - 1];
            if z[i] < need { z[i] = need; moved = true; }
        }
        for i in (1..n).rev() {
            let allow = z[i] - glos[i - 1];
            if z[i - 1] > allow { z[i - 1] = allow; moved = true; }
        }
        if !moved { break; }
    }
    // 池分组：连续等值且未被缺芯下界分隔的节点才归入同一 PAVA 池。
    // 注意必须看原始年龄差（z），不能看变换后的 y：缺芯两侧在 y 空间
    // 可能恰好同值，但它们在物理上被正下界分隔，绝不能共享依赖池。
    let mut groups = vec![0usize; n];
    for i in 1..n {
        let gap_split = glos[i - 1] > EPS;
        let split = gap_split || (z[i] - z[i - 1]).abs() > 1e-7;
        groups[i] = groups[i - 1] + if split { 1 } else { 0 };
    }

    // 解析化收尾：在已识别的活动集（PAVA 池 + 硬盒夹点）上求精。
    z = polish(&z, targets, weights, hard_lo, hard_hi, glos, &groups);
    for i in 0..n {
        z[i] = z[i].max(if hard_lo[i].is_finite() { hard_lo[i] } else { f64::NEG_INFINITY });
        z[i] = z[i].min(if hard_hi[i].is_finite() { hard_hi[i] } else { f64::INFINITY });
    }
    (z, groups)
}

// ---------- 蒙特卡洛主流程 ----------

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// 一次完整求解。不可行时返回带最小冲突集的结果（不产生分位数）。
pub fn solve(input: &SolveInput) -> SolveResult {
    let problem = match build_problem(input) {
        Ok(p) => p,
        Err(e) => {
            return SolveResult {
                feasible: false,
                nodes: vec![],
                edges: vec![],
                conflict: Some(ConflictSet {
                    reason: e,
                    minimal: false,
                    markers: vec![],
                    gaps: vec![],
                }),
                warnings: vec![],
                draws: 0,
            }
        }
    };

    if let Some(conflict) = check_feasible(&problem) {
        return SolveResult {
            feasible: false,
            nodes: vec![],
            edges: vec![],
            conflict: Some(conflict),
            warnings: vec![],
            draws: 0,
        };
    }

    let n = problem.nodes.len();
    let active: Vec<&Marker> = problem
        .markers
        .iter()
        .filter(|m| !m.excluded)
        .collect();
    let node_of_depth: BTreeMap<i64, usize> = problem
        .nodes
        .iter()
        .enumerate()
        .map(|(i, nd)| ((nd.depth * 1e9).round() as i64, i))
        .collect();
    let node_index = |depth: f64| -> usize {
        *node_of_depth
            .get(&((depth * 1e9).round() as i64))
            .expect("node index")
    };

    // 交换组：按深度排序后，把组内分布参数轮换（swapped 时逆序）。
    let mut effective: std::collections::BTreeMap<String, (f64, f64, f64, Vec<f64>)> =
        BTreeMap::new();
    for state in &input.exchanges {
        let members: Vec<&&Marker> = active
            .iter()
            .filter(|m| m.exchange_group.as_deref() == Some(state.group.as_str()))
            .collect();
        let mut by_depth: Vec<&&Marker> = members.clone();
        by_depth.sort_by(|a, b| a.depth.partial_cmp(&b.depth).unwrap());
        let k = by_depth.len();
        if k >= 2 {
            for (idx, m) in by_depth.iter().enumerate() {
                let src = if state.swapped {
                    by_depth[(idx + k - 1) % k]
                } else {
                    *m
                };
                effective.insert(
                    m.id.clone(),
                    (
                        src.mean.unwrap_or((src.lo + src.hi) / 2.0),
                        src.sd.unwrap_or((src.hi - src.lo) / 4.0),
                        src.weight,
                        src.alt_means.clone(),
                    ),
                );
            }
        }
    }

    let mut age_samples = vec![vec![0f64; input.draws]; n];
    let mut pool_marker_deps = vec![BTreeMap::<String, ()>::new(); n];
    let mut pool_gap_deps = vec![BTreeMap::<String, ()>::new(); n];

    let hard_lo: Vec<f64> = problem.nodes.iter().map(|nd| nd.lo).collect();
    let hard_hi: Vec<f64> = problem.nodes.iter().map(|nd| nd.hi).collect();
    let glos: Vec<f64> = problem.edges.iter().map(|e| e.glo).collect();

    for draw in 0..input.draws {
        let mut targets = vec![0.0; n];
        let mut weights = vec![1e-6; n];
        for m in &active {
            let idx = node_index(m.depth);
            let key = format!("{}:{}:v{}", m.id, draw, input.solver_version);
            let mut rng = Rng::derive(input.seed, &key);
            let (mean, sd, weight, alt) = if let Some(eff) = effective.get(&m.id) {
                eff.clone()
            } else {
                (
                    m.mean.unwrap_or((m.lo + m.hi) / 2.0),
                    m.sd.unwrap_or((m.hi - m.lo) / 4.0),
                    m.weight,
                    m.alt_means.clone(),
                )
            };
            let sample = if !alt.is_empty() {
                alt[rng.choice(alt.len())]
            } else if m.hard && m.sd.is_none() {
                m.lo + rng.uniform01() * (m.hi - m.lo)
            } else {
                rng.truncated_normal(mean, sd, m.lo, m.hi)
            };
            targets[idx] = sample;
            weights[idx] = weight.max(1e-4);
        }

        // 无观测节点：以相邻锦标样本按深度线性插值（两端恒值外延）作为弱目标，
        // 避免任意的 0 目标在加权最小二乘里轻微拖偏相邻解。
        let mut marker_at: Vec<Option<(f64, f64)>> = vec![None; n];
        for m in &active {
            marker_at[node_index(m.depth)] = Some((m.depth, targets[node_index(m.depth)]));
        }
        let known: Vec<(f64, f64)> = marker_at.iter().flatten().copied().collect();
        for (i, slot) in marker_at.iter().enumerate() {
            if slot.is_some() || known.is_empty() {
                continue;
            }
            let d = problem.nodes[i].depth;
            let target = if d <= known[0].0 {
                known[0].1
            } else if d >= known[known.len() - 1].0 {
                known[known.len() - 1].1
            } else {
                let k = known.iter().position(|p| p.0 >= d).unwrap();
                let (d1, a1) = known[k - 1];
                let (d2, a2) = known[k];
                a1 + (a2 - a1) * (d - d1) / (d2 - d1)
            };
            targets[i] = target;
        }

        let (z, groups) = dykstra_solve(&targets, &weights, &hard_lo, &hard_hi, &glos);
        for (i, value) in z.iter().enumerate() {
            age_samples[i][draw] = *value;
        }

        // 依赖：同一 PAVA 池内所有软/硬锦标、缺芯边，以及节点自身硬锦标。
        let mut by_group: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, g) in groups.iter().enumerate() {
            by_group.entry(*g).or_default().push(i);
        }
        for members in by_group.values() {
            let mut mids: BTreeMap<String, ()> = BTreeMap::new();
            let mut gids: BTreeMap<String, ()> = BTreeMap::new();
            for &i in members {
                for id in &problem.nodes[i].hard_markers {
                    mids.insert(id.clone(), ());
                }
                for id in &problem.nodes[i].soft_markers {
                    mids.insert(id.clone(), ());
                }
                // 缺芯下界是“向下传播”的约束：只有缺芯底边上的节点（i 是
                // gap 边的右端点）才依赖该缺芯；顶边节点不受该缺芯影响。
                if i > 0 {
                    if let Some(g) = &problem.edges[i - 1].gap_of {
                        gids.insert(g.clone(), ());
                    }
                }
            }
            for &i in members {
                for k in mids.keys() {
                    pool_marker_deps[i].insert(k.clone(), ());
                }
                for k in gids.keys() {
                    pool_gap_deps[i].insert(k.clone(), ());
                }
            }
        }
    }

    // 边速率（必须在分位数排序前计算，保持同一次抽样的节点配对）
    let mut edge_rates: Vec<Vec<f64>> = Vec::with_capacity(problem.edges.len());
    for (k, edge) in problem.edges.iter().enumerate() {
        let mut rates = vec![0f64; input.draws];
        for d in 0..input.draws {
            let delta = age_samples[k + 1][d] - age_samples[k][d];
            rates[d] = if edge.gap_of.is_some() {
                delta
            } else {
                delta / edge.depth_span
            };
        }
        rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        edge_rates.push(rates);
    }

    // 依赖闭包扩张：无自身锦标的自由节点（含缺芯边界面）通过插值目标与
    // 单调性传播受到邻近锦标/缺芯影响。保守地取最近上、下“有锦标”节点的
    // 并集，以及全部缺芯键的闭包——宁可多重算失效，不可错误复用。
    let known_anchor: Vec<usize> = problem
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, nd)| !nd.hard_markers.is_empty() || !nd.soft_markers.is_empty())
        .map(|(i, _)| i)
        .collect();
    for i in 0..n {
        let nd = &problem.nodes[i];
        let own = !nd.hard_markers.is_empty() || !nd.soft_markers.is_empty();
        if !own {
            let prev = known_anchor.iter().copied().rev().find(|&k| k <= i);
            let next = known_anchor.iter().copied().find(|&k| k >= i);
            for k in [prev, next].iter().flatten().copied() {
                let mids: Vec<String> = pool_marker_deps[k].keys().cloned().collect();
                let gids: Vec<String> = pool_gap_deps[k].keys().cloned().collect();
                for id in mids { pool_marker_deps[i].insert(id, ()); }
                for g in gids { pool_gap_deps[i].insert(g, ()); }
            }
        }
    }
    // 缺芯下界的影响沿单调性向下传播：若某节点依赖缺芯，则其下游节点也依赖，
    // 除非该节点有自身硬锦标且未与缺口同池（此时硬锚点可“截断”传播——
    // 为保证安全，这里仍保守传播到下一个硬锦标为止）。
    let mut carry: BTreeMap<String, ()> = BTreeMap::new();
    for i in 0..n {
        for g in pool_gap_deps[i].keys() {
            carry.insert(g.clone(), ());
        }
        let nd_has_hard = !problem.nodes[i].hard_markers.is_empty();
        if !nd_has_hard {
            for g in carry.keys() {
                pool_gap_deps[i].insert(g.clone(), ());
            }
        }
    }

    // 分位数与依赖
    let mut nodes = Vec::with_capacity(n);
    for (i, samples) in age_samples.iter_mut().enumerate() {
        let nd = &problem.nodes[i];
        let mut dep_markers: Vec<String> = nd
            .hard_markers
            .iter()
            .chain(nd.soft_markers.iter())
            .cloned()
            .collect();
        for k in pool_marker_deps[i].keys() {
            dep_markers.push(k.clone());
        }
        dep_markers.sort();
        dep_markers.dedup();
        nodes.push(NodeQ {
            depth: nd.depth,
            q05: round6(quantile(samples, 0.05)),
            median: round6(quantile(samples, 0.50)),
            q95: round6(quantile(samples, 0.95)),
            dep_markers,
            dep_gaps: pool_gap_deps[i].keys().cloned().collect(),
        });
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // 排序放在读取原始配对之后；分位数在构建 NodeQ 时已计算。
        let qv = |q: f64| round6(quantile(samples, q));
        nodes[i].q05 = qv(0.05);
        nodes[i].median = qv(0.50);
        nodes[i].q95 = qv(0.95);
    }

    let mut edges = Vec::with_capacity(problem.edges.len());
    for (k, edge) in problem.edges.iter().enumerate() {
        let rates = &edge_rates[k];
        let mut dm = nodes[k].dep_markers.clone();
        dm.extend(nodes[k + 1].dep_markers.iter().cloned());
        dm.sort();
        dm.dedup();
        let mut dg = nodes[k].dep_gaps.clone();
        dg.extend(nodes[k + 1].dep_gaps.iter().cloned());
        if let Some(g) = &edge.gap_of {
            dg.push(g.clone());
        }
        dg.sort();
        dg.dedup();
        edges.push(EdgeQ {
            depth_from: problem.nodes[k].depth,
            depth_to: problem.nodes[k + 1].depth,
            rate_median: round6(quantile(rates, 0.50)),
            rate_q05: round6(quantile(rates, 0.05)),
            rate_q95: round6(quantile(rates, 0.95)),
            is_gap: edge.gap_of.is_some(),
            gap_segment: edge.gap_of.clone(),
            dep_markers: dm,
            dep_gaps: dg,
        });
    }

    // 相触提示：若存在硬区间恰好相触，给一条可审计 warning。
    let mut warnings = Vec::new();
    for nd in &problem.nodes {
        if nd.hard_markers.len() >= 2 && (nd.lo - nd.hi).abs() <= EPS {
            warnings.push(format!("深度 {} 的硬约束恰好相触（可行解）", nd.depth));
        }
    }

    SolveResult {
        feasible: true,
        nodes,
        edges,
        conflict: None,
        warnings,
        draws: input.draws,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn infv(n: usize) -> Vec<f64> {
        vec![f64::INFINITY; n]
    }
    fn negfv(n: usize) -> Vec<f64> {
        vec![f64::NEG_INFINITY; n]
    }

    #[test]
    fn isolated_soft_targets_hit_exactly() {
        // 只有两个有目标节点，自由节点弱权重；无硬盒、无 gap。
        let targets = vec![0.0, 50.0, 70.0, 90.0];
        let weights = vec![1000.0, 10.0, 1e-6, 10.0];
        let (z, _) = dykstra_solve(&targets, &weights, &negfv(4), &infv(4), &[0.0, 0.0, 0.0]);
        assert!((z[0] - 0.0).abs() < 1e-6, "z0={}", z[0]);
        assert!((z[1] - 50.0).abs() < 1e-6, "z1={}", z[1]);
        assert!((z[3] - 90.0).abs() < 1e-6, "z3={}", z[3]);
        assert!(z[2] >= z[1] - 1e-9 && z[2] <= z[3] + 1e-9);
    }

    #[test]
    fn hard_box_clamp_is_exact() {
        let targets = vec![0.0, 5.0, 200.0];
        let weights = vec![1.0, 1.0, 1.0];
        let lo = vec![f64::NEG_INFINITY; 3];
        let mut hi = infv(3);
        hi[2] = 160.0;
        let (z, _) = dykstra_solve(&targets, &weights, &lo, &hi, &[0.0, 0.0]);
        assert!((z[2] - 160.0).abs() < 1e-7, "z2={}", z[2]);
        assert!(z[0] <= z[1] && z[1] <= z[2]);
    }

    #[test]
    fn gap_offsets_and_feasibility() {
        let targets = vec![0.0, 100.0, 105.0];
        let weights = vec![1.0, 100.0, 100.0];
        let (z, _) = dykstra_solve(
            &targets,
            &weights,
            &negfv(3),
            &infv(3),
            &[0.0, 30.0],
        );
        assert!(z[2] >= z[1] + 30.0 - 1e-9);
    }

    #[test]
    fn touching_intervals_are_feasible() {
        let input = crate::fixtures::seed_input();
        let r = solve(&input);
        assert!(r.feasible);
    }
}
