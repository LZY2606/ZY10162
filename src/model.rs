//! Domain model: evidence records, derived depth grid, hard-constraint
//! envelopes and the minimum conflict set computation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const EPS: f64 = 1e-9;
pub const SURFACE_ID: &str = "__surface__";
pub const DEFAULT_RATE_MEAN: f64 = 6.0;
pub const DEFAULT_RATE_SIGMA: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Layer,
    Ash,
    Isotope,
    Hard,
    Gap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub segment: String,
    pub kind: EvidenceKind,
    pub depth: f64,
    pub enabled: bool,
    pub mean: Option<f64>,
    pub sigma: Option<f64>,
    pub age_lo: Option<f64>,
    pub age_hi: Option<f64>,
    pub gap_end: Option<f64>,
    #[serde(default)]
    pub start_depth: Option<f64>,
    pub gap_mean: Option<f64>,
    pub gap_sigma: Option<f64>,
    pub label: String,
}

impl Evidence {
    pub fn gap_min_years(&self) -> f64 {
        let mean = self.gap_mean.unwrap_or(8.0);
        let sigma = self.gap_sigma.unwrap_or(3.0);
        (mean - 2.0 * sigma).max(1.0)
    }

    pub fn is_gap(&self) -> bool {
        self.kind == EvidenceKind::Gap
    }

    pub fn is_hard(&self) -> bool {
        self.kind == EvidenceKind::Hard
    }

    pub fn soft_target(&self) -> Option<(f64, f64)> {
        if matches!(self.kind, EvidenceKind::Ash | EvidenceKind::Isotope) {
            if let (Some(mean), Some(sigma)) = (self.mean, self.sigma) {
                if sigma.is_finite() && sigma > 0.0 {
                    return Some((mean, sigma));
                }
            }
        }
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub seed: u64,
    pub draws: u32,
    pub surface_age: f64,
    pub rate_mean: f64,
    pub rate_sigma: f64,
    pub layer_kappa: f64,
    pub prior_kappa: f64,
    pub quantiles: Vec<f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            seed: 20260921,
            draws: 600,
            surface_age: 0.0,
            rate_mean: DEFAULT_RATE_MEAN,
            rate_sigma: DEFAULT_RATE_SIGMA,
            layer_kappa: 14.0,
            prior_kappa: 8.0,
            quantiles: vec![0.025, 0.5, 0.975],
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Node {
    pub key: String,
    pub depth: f64,
    pub hard_lo: f64,
    pub hard_hi: f64,
    pub hard_ids: Vec<String>,
    pub soft_mean: Option<f64>,
    pub soft_sigma: Option<f64>,
    pub is_surface: bool,
    pub anchor: bool,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    pub key: String,
    pub from: usize,
    pub to: usize,
    pub depth_lo: f64,
    pub depth_hi: f64,
    pub is_gap: bool,
    pub gap_id: Option<String>,
    pub gap_min_years: f64,
    pub gap_mean: Option<f64>,
    pub gap_sigma: Option<f64>,
    pub normal_thickness: f64,
    pub layer_alpha: f64,
}

#[derive(Debug)]
pub struct Model {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub hard_ids: Vec<String>,
    pub gaps: Vec<Evidence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Conflict {
    pub at_depth: f64,
    pub node_key: String,
    pub lo: f64,
    pub hi: f64,
    pub hard_ids: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct OrderedFloat(pub f64);
impl Eq for OrderedFloat {}
#[allow(clippy::derive_ord_xor_partial_ord_impl)]
impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).expect("finite depth")
    }
}

fn fail(msg: impl Into<String>) -> String {
    msg.into()
}

impl Model {
    pub fn build(evidence: &[Evidence], settings: &Settings) -> Result<Model, String> {
        let enabled: Vec<&Evidence> = evidence.iter().filter(|e| e.enabled).collect();

        let mut gaps: Vec<Evidence> = Vec::new();
        for ev in &enabled {
            if ev.depth < 0.0 || !ev.depth.is_finite() {
                return Err(fail(format!("证据 {} 的深度必须为非负有限数", ev.id)));
            }
            if ev.is_gap() {
                let end = ev
                    .gap_end
                    .ok_or_else(|| fail(format!("缺芯 {} 缺少结束深度", ev.id)))?;
                if !end.is_finite() || end <= ev.depth {
                    return Err(fail(format!(
                        "缺芯 {} 的结束深度必须严格大于起始深度",
                        ev.id
                    )));
                }
                gaps.push((*ev).clone());
            }
            if ev.is_hard() {
                let lo = ev
                    .age_lo
                    .ok_or_else(|| fail(format!("硬约束 {} 缺少年龄下限", ev.id)))?;
                let hi = ev
                    .age_hi
                    .ok_or_else(|| fail(format!("硬约束 {} 缺少年龄上限", ev.id)))?;
                if hi < lo {
                    return Err(fail(format!("硬约束 {} 的上限小于下限", ev.id)));
                }
            }
        }
        gaps.sort_by(|a, b| a.depth.partial_cmp(&b.depth).unwrap());
        for pair in gaps.windows(2) {
            if pair[1].depth < pair[0].gap_end.unwrap() - EPS {
                return Err(fail(format!(
                    "缺芯 {} 与 {} 的深度区间重叠",
                    pair[0].id, pair[1].id
                )));
            }
        }

        // Depth grid: surface, every evidence depth, and every gap endpoint.
        let mut grid: Vec<f64> = vec![0.0];
        for ev in &enabled {
            grid.push(ev.depth);
            if ev.is_gap() {
                grid.push(ev.gap_end.unwrap());
            }
        }
        grid.sort_by(|a, b| a.partial_cmp(b).unwrap());
        grid.dedup_by(|a, b| (*a - *b).abs() <= EPS);

        for ev in &enabled {
            if ev.is_gap() {
                continue;
            }
            let inside = gaps
                .iter()
                .any(|g| ev.depth > g.depth + EPS && ev.depth < g.gap_end.unwrap() - EPS);
            if inside {
                return Err(fail(format!("证据 {} 位于缺芯内部，无法约束年龄", ev.id)));
            }
        }

        let mut by_depth: BTreeMap<OrderedFloat, Vec<&Evidence>> = BTreeMap::new();
        for ev in &enabled {
            by_depth.entry(OrderedFloat(ev.depth)).or_default().push(ev);
        }

        let mut nodes: Vec<Node> = Vec::new();
        for (i, &depth) in grid.iter().enumerate() {
            let mut node = Node {
                key: format!("n{:03}", i),
                depth,
                hard_lo: f64::NEG_INFINITY,
                hard_hi: f64::INFINITY,
                hard_ids: Vec::new(),
                soft_mean: None,
                soft_sigma: None,
                is_surface: depth == 0.0,
                anchor: depth == 0.0,
                label: String::new(),
            };
            let mut labels: Vec<String> = Vec::new();
            let mut soft_weight = 0.0f64;
            let mut soft_weighted = 0.0f64;
            if let Some(evs) = by_depth.get(&OrderedFloat(depth)) {
                let mut primary: Option<String> = None;
                for ev in evs {
                    if primary.is_none() {
                        primary = Some(ev.id.clone());
                    }
                    if ev.is_hard() {
                        node.hard_lo = node.hard_lo.max(ev.age_lo.unwrap());
                        node.hard_hi = node.hard_hi.min(ev.age_hi.unwrap());
                        node.hard_ids.push(ev.id.clone());
                    }
                    if let Some((mean, sigma)) = ev.soft_target() {
                        let w = 1.0 / (sigma * sigma);
                        soft_weight += w;
                        soft_weighted += w * mean;
                    }
                    if !ev.label.is_empty() {
                        labels.push(ev.label.clone());
                    }
                }
                if !node.is_surface {
                    node.key = primary.unwrap_or_else(|| {
                        gaps.iter()
                            .find(|g| (g.gap_end.unwrap() - depth).abs() <= EPS)
                            .map(|g| format!("gap-end:{}", g.id))
                            .unwrap_or_else(|| node.key.clone())
                    });
                }
            }
            if soft_weight > 0.0 {
                node.soft_sigma = Some(1.0 / soft_weight.sqrt());
                node.soft_mean = Some(soft_weighted / soft_weight);
                node.anchor = true;
            }
            if !node.hard_ids.is_empty() {
                node.anchor = true;
            }
            labels.sort();
            labels.dedup();
            node.label = labels.join(" / ");
            nodes.push(node);
        }

        nodes[0].key = SURFACE_ID.to_string();
        nodes[0].hard_lo = settings.surface_age;
        nodes[0].hard_hi = settings.surface_age;
        nodes[0].hard_ids.push(SURFACE_ID.to_string());

        // One edge per consecutive node pair. A pair matching a gap is a
        // non-material edge carrying a minimum elapsed duration.
        let mut edges: Vec<Edge> = Vec::new();
        for (idx, pair) in nodes.windows(2).enumerate() {
            let lo = pair[0].depth;
            let hi = pair[1].depth;
            if let Some(gap) = gaps.iter().find(|g| g.depth == lo && g.gap_end == Some(hi)) {
                edges.push(Edge {
                    key: format!("gap:{}", gap.id),
                    from: idx,
                    to: idx + 1,
                    depth_lo: lo,
                    depth_hi: hi,
                    is_gap: true,
                    gap_id: Some(gap.id.clone()),
                    gap_min_years: gap.gap_min_years(),
                    gap_mean: Some(gap.gap_mean.unwrap_or(8.0)),
                    gap_sigma: Some(gap.gap_sigma.unwrap_or(3.0)),
                    normal_thickness: 0.0,
                    layer_alpha: 0.0,
                });
            } else if gaps
                .iter()
                .any(|g| lo >= g.depth - EPS && hi <= g.gap_end.unwrap() + EPS)
            {
                return Err(fail(
                    "缺芯内部出现了深度节点，年龄不应在缺芯内插值".to_string(),
                ));
            } else {
                edges.push(Edge {
                    key: format!("edge:{}", nodes[idx].key),
                    from: idx,
                    to: idx + 1,
                    depth_lo: lo,
                    depth_hi: hi,
                    is_gap: false,
                    gap_id: None,
                    gap_min_years: 0.0,
                    gap_mean: None,
                    gap_sigma: None,
                    normal_thickness: hi - lo,
                    layer_alpha: settings.prior_kappa * (hi - lo),
                });
            }
        }

        // Layer-count concentration is added to covered normal edges.
        for ev in enabled.iter().filter(|e| e.kind == EvidenceKind::Layer) {
            let start = ev.start_depth.unwrap_or(0.0);
            let count = ev.mean.unwrap_or(0.0).max(0.0);
            let parts = edges
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    !e.is_gap && e.depth_lo < ev.depth - EPS && e.depth_hi > start + EPS
                })
                .map(|(i, e)| (i, e.normal_thickness))
                .collect::<Vec<_>>();
            let total: f64 = parts.iter().map(|(_, t)| *t).sum();
            if total <= EPS && count > 0.0 {
                return Err(fail(format!("季节层证据 {} 未覆盖任何实芯", ev.id)));
            }
            for (idx, thickness) in parts {
                let share = if total > EPS {
                    count * thickness / total
                } else {
                    0.0
                };
                edges[idx].layer_alpha += settings.layer_kappa * share;
            }
        }

        let mut hard_ids: Vec<String> = enabled
            .iter()
            .filter(|e| e.is_hard())
            .map(|e| e.id.clone())
            .collect();
        hard_ids.sort();
        hard_ids.dedup();

        Ok(Model {
            nodes,
            edges,
            hard_ids,
            gaps,
        })
    }

    /// Earliest feasible ages after forward propagation through monotone
    /// non-negative increments and missing-segment minimum durations.
    pub fn lower_envelope(&self, settings: &Settings) -> Vec<f64> {
        let mut lo = vec![f64::NEG_INFINITY; self.nodes.len()];
        lo[0] = settings.surface_age;
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                lo[i] = lo[i].max(node.hard_lo);
            }
        }
        for edge in &self.edges {
            let candidate = lo[edge.from] + edge.gap_min_years;
            if candidate > lo[edge.to] {
                lo[edge.to] = candidate;
            }
        }
        lo
    }

    /// Latest feasible ages after backward propagation.
    pub fn upper_envelope(&self, settings: &Settings) -> Vec<f64> {
        let mut hi = vec![f64::INFINITY; self.nodes.len()];
        hi[0] = settings.surface_age;
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                hi[i] = hi[i].min(node.hard_hi);
            }
        }
        for edge in self.edges.iter().rev() {
            let candidate = hi[edge.to] - edge.gap_min_years;
            if candidate < hi[edge.from] {
                hi[edge.from] = candidate;
            }
        }
        hi
    }

    pub fn feasibility(&self, settings: &Settings) -> (bool, Vec<Conflict>) {
        let lo = self.lower_envelope(settings);
        let hi = self.upper_envelope(settings);
        let mut conflicts = Vec::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if lo[i] > hi[i] + EPS {
                let reason = if node.hard_lo > node.hard_hi + EPS {
                    "同深度硬约束区间无交集".to_string()
                } else {
                    "单调不减与缺芯最小时长使硬约束不可同时满足".to_string()
                };
                conflicts.push(Conflict {
                    at_depth: node.depth,
                    node_key: node.key.clone(),
                    lo: lo[i],
                    hi: hi[i],
                    hard_ids: node.hard_ids.clone(),
                    reason,
                });
            }
        }
        (conflicts.is_empty(), conflicts)
    }

    /// Minimum unsat subset via iterative deletion. The surface fixed age
    /// and gap minima are permanent; only removable hard intervals enter
    /// the probe sets. The returned set is inclusion-minimal.
    pub fn minimum_conflict_set(evidence: &[Evidence], settings: &Settings) -> Vec<String> {
        let base = Model::build(evidence, settings).expect("validated geometry");
        let mut remaining: Vec<String> = base.hard_ids.clone();
        loop {
            let mut dropped = false;
            for candidate in remaining.clone() {
                let probe_ids: Vec<String> = remaining
                    .iter()
                    .filter(|id| id.as_str() != candidate)
                    .cloned()
                    .collect();
                let kept: Vec<Evidence> = evidence
                    .iter()
                    // Surface and non-hard evidence stay permanent.
                    .filter(|e| !e.is_hard() || probe_ids.iter().any(|id| id == &e.id))
                    .cloned()
                    .collect();
                let still_unsat = match Model::build(&kept, settings) {
                    Ok(model) => !model.feasibility(settings).0,
                    Err(_) => true,
                };
                // If the candidate is unnecessary, the rest remains unsat:
                // drop it for good; otherwise it belongs to the MUS.
                if still_unsat {
                    remaining = probe_ids;
                    dropped = true;
                    break;
                }
            }
            if !dropped {
                break;
            }
        }
        remaining.sort();
        remaining.dedup();
        remaining
    }
}
