//! Deterministic ensemble sampler for the monotonic depth-age curve.
//!
//! Anchor nodes (hard intervals and soft age points) are drawn from their
//! boxes/truncated normals in depth order; increments between anchors are
//! split with a Dirichlet prior weighted by local layer counts, and missing
//! segments are drawn from positive gamma durations.

use crate::model::{Conflict, Evidence, Model, Node, Settings, EPS};
use crate::rng::Rng;
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct NodeResult {
    pub key: String,
    pub depth: f64,
    pub median: f64,
    pub quantiles: Vec<f64>,
    pub soft_residual_sigma: Option<f64>,
    pub in_hard_box: bool,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct EdgeResult {
    pub key: String,
    pub depth_lo: f64,
    pub depth_hi: f64,
    pub is_gap: bool,
    pub gap_id: Option<String>,
    pub median_duration: f64,
    pub median_rate: f64,
}

pub struct RunOutput {
    pub feasible: bool,
    pub run_id: String,
    pub record: Value,
    pub nodes: Vec<NodeResult>,
    pub conflicts: Vec<Conflict>,
    pub minimum_conflict_set: Vec<String>,
}

pub fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let h = (sorted.len() as f64 - 1.0) * p;
    let lower = h.floor() as usize;
    let upper = h.ceil() as usize;
    let frac = h - lower as f64;
    sorted[lower] * (1.0 - frac) + sorted[upper] * frac
}

struct AnchorCtx {
    index: usize,
    effective_lo: f64,
    effective_hi: f64,
}

fn expected_increment(edge: &crate::model::Edge, settings: &Settings) -> f64 {
    if edge.is_gap {
        edge.gap_mean.unwrap()
    } else if edge.layer_alpha > settings.prior_kappa * edge.normal_thickness + EPS {
        // Derive a rate from the layer-count prior; alpha = kappa * count.
        let counts = (edge.layer_alpha - settings.prior_kappa * edge.normal_thickness)
            / settings.layer_kappa;
        counts.max(edge.normal_thickness * 0.05)
    } else {
        settings.rate_mean * edge.normal_thickness
    }
}

fn anchor_ages(model: &Model, settings: &Settings) -> Vec<AnchorCtx> {
    let lower = model.lower_envelope(settings);
    let upper = model.upper_envelope(settings);
    model
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.anchor)
        .map(|(index, _node)| AnchorCtx {
            index,
            effective_lo: lower[index],
            effective_hi: upper[index],
        })
        .collect()
}

fn draw_anchor_age(
    rng: &mut Rng,
    node: &Node,
    prev_age: f64,
    lo: f64,
    hi: f64,
    settings: &Settings,
) -> f64 {
    let lo = lo.max(prev_age);
    let hi = hi.max(lo);
    if node.is_surface {
        return settings.surface_age;
    }
    match (node.soft_mean, node.soft_sigma) {
        (Some(mean), Some(sigma)) => {
            let value = rng.truncated_normal(mean, sigma, lo, hi);
            if value < lo - EPS || value > hi + EPS {
                lo
            } else {
                value
            }
        }
        _ => rng.uniform_range(lo, hi),
    }
}

fn draw_span(
    model: &Model,
    settings: &Settings,
    rng_base: u64,
    edge_ids: &[usize],
    ages: &mut [f64],
    from_age: f64,
    to_age: Option<f64>,
    min_to_age: f64,
) -> Option<f64> {
    ages[model.edges[edge_ids[0]].from] = from_age;
    let mut gap_total = 0.0f64;
    let mut gap_durations: Vec<(usize, f64)> = Vec::new();

    if let Some(end) = to_age {
        // Fixed-budget span: draw gap durations first, then distribute the
        // remaining budget over material edges with a layer-weighted
        // Dirichlet.
        for &ei in edge_ids {
            let edge = &model.edges[ei];
            if !edge.is_gap {
                continue;
            }
            let mut rng = Rng::derived(rng_base, &edge.key);
            let mean = edge.gap_mean.unwrap();
            let sigma = edge.gap_sigma.unwrap();
            let lo = edge.gap_min_years;
            let remaining = end - from_age - gap_total;
            if remaining < lo - EPS {
                return None;
            }
            let duration = rng.truncated_gamma(mean, sigma, lo, remaining);
            if duration < lo - EPS || duration > remaining + EPS {
                return None;
            }
            gap_total += duration;
            gap_durations.push((ei, duration));
        }
        let budget = (end - from_age - gap_total).max(0.0);

        let normal: Vec<usize> = edge_ids
            .iter()
            .copied()
            .filter(|ei| !model.edges[*ei].is_gap)
            .collect();
        let alphas: Vec<f64> = normal
            .iter()
            .map(|ei| model.edges[*ei].layer_alpha.max(0.05))
            .collect();
        let sum_alpha: f64 = alphas.iter().sum();
        let gammas: Vec<f64> = normal
            .iter()
            .zip(alphas.iter())
            .map(|(ei, alpha)| {
                let mut rng = Rng::derived(rng_base, &model.edges[*ei].key);
                rng.gamma(*alpha, 1.0)
            })
            .collect();
        let gsum: f64 = gammas.iter().sum();

        // Walk edges in depth order and assign ages.
        for &ei in edge_ids {
            if let Some((_, duration)) = gap_durations.iter().find(|(id, _)| *id == ei) {
                ages[model.edges[ei].to] = ages[model.edges[ei].from] + *duration;
            } else {
                let pos = normal.iter().position(|id| *id == ei).unwrap();
                let fraction = if gsum > EPS { gammas[pos] / gsum } else { 0.0 };
                let _ = sum_alpha;
                let duration = budget * fraction;
                ages[model.edges[ei].to] = ages[model.edges[ei].from] + duration;
            }
        }
        Some(ages[model.edges[*edge_ids.last().unwrap()].to])
    } else {
        // Open-ended span (above the first anchor or below the last one):
        // each edge is an independent positive draw.
        for &ei in edge_ids {
            let edge = &model.edges[ei];
            let mut rng = Rng::derived(rng_base, &edge.key);
            let duration = if edge.is_gap {
                let mean = edge.gap_mean.unwrap();
                let sigma = edge.gap_sigma.unwrap();
                rng.gamma((mean / sigma).powi(2), sigma.powi(2) / mean)
                    .max(edge.gap_min_years)
            } else {
                let expected = expected_increment(edge, settings);
                let sigma = edge_rate_sigma(edge, settings) * edge.normal_thickness;
                let shape = (expected / sigma.max(1e-6)).powi(2).max(1.0);
                rng.gamma(shape, expected / shape)
            };
            ages[edge.to] = ages[edge.from] + duration;
        }
        Some(ages[model.edges[*edge_ids.last().unwrap()].to].max(min_to_age))
    }
}

fn edge_rate_sigma(edge: &crate::model::Edge, settings: &Settings) -> f64 {
    if edge.layer_alpha > settings.prior_kappa * edge.normal_thickness + EPS {
        // Layer counts pin local rates tightly relative to the fallback.
        settings.rate_sigma * 0.35
    } else {
        settings.rate_sigma
    }
}

fn draw_once(
    model: &Model,
    settings: &Settings,
    anchors: &[AnchorCtx],
    draw_index: u32,
) -> Option<Vec<f64>> {
    let mut ages = vec![f64::NAN; model.nodes.len()];
    ages[0] = settings.surface_age;

    let mut anchor_age_values: Vec<f64> = Vec::with_capacity(anchors.len());
    let mut prev_age = settings.surface_age;
    for ctx in anchors {
        let node = &model.nodes[ctx.index];
        let mut rng = Rng::derived(
            settings.seed,
            &format!("anchor:{}:{}:{}", node.key, draw_index, settings.draws),
        );
        let value = draw_anchor_age(
            rng_node_seed(&mut rng),
            node,
            prev_age,
            ctx.effective_lo,
            ctx.effective_hi,
            settings,
        );
        if value < ctx.effective_lo - 1e-7 || value > ctx.effective_hi + 1e-7 {
            return None;
        }
        anchor_age_values.push(value);
        prev_age = value;
    }

    // Fixed-budget spans between consecutive anchor nodes.
    let anchor_indices: Vec<usize> = anchors.iter().map(|a| a.index).collect();
    for pair_pos in 1..anchor_indices.len() {
        let prev_index = anchor_indices[pair_pos - 1];
        let end_index = anchor_indices[pair_pos];
        let edge_ids: Vec<usize> = model
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from >= prev_index && e.to <= end_index)
            .map(|(i, _)| i)
            .collect();
        if edge_ids.is_empty() {
            continue;
        }
        let result = draw_span(
            model,
            settings,
            settings.seed,
            &edge_ids,
            &mut ages,
            anchor_age_values[pair_pos - 1],
            Some(anchor_age_values[pair_pos]),
            anchor_age_values[pair_pos],
        );
        if result.is_none() {
            return None;
        }
    }

    // Trailing open-ended span after the last anchor.
    let last_anchor = *anchor_indices.last().unwrap_or(&0);
    let tail: Vec<usize> = model
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.from >= last_anchor)
        .map(|(i, _)| i)
        .collect();
    if !tail.is_empty() {
        if draw_span(
            model,
            settings,
            settings.seed,
            &tail,
            &mut ages,
            anchor_age_values
                .last()
                .copied()
                .unwrap_or(settings.surface_age),
            None,
            f64::NEG_INFINITY,
        )
        .is_none()
        {
            return None;
        }
    }

    // Monotonicity guard (sampling math should already guarantee it).
    for edge in &model.edges {
        if ages[edge.to] < ages[edge.from] + edge.gap_min_years - 1e-7 {
            return None;
        }
    }
    Some(ages)
}

fn rng_node_seed(rng: &mut Rng) -> &mut Rng {
    rng
}

fn canonical_input(evidence: &[Evidence], settings: &Settings) -> (String, String) {
    let mut evs: Vec<Value> = evidence
        .iter()
        .map(|e| serde_json::to_value(e).expect("serialize evidence"))
        .collect();
    evs.sort_by(|a, b| {
        a.get("id")
            .and_then(|v| v.as_str())
            .cmp(&b.get("id").and_then(|v| v.as_str()))
    });
    let payload = json!({
        "evidence": evs,
        "settings": serde_json::to_value(settings).expect("settings"),
        "schema": "hanleng-nianchi/run/v1",
    });
    let canonical = serde_json::to_string(&payload).expect("canonical json");
    let digest = sha2::Sha256::digest(canonical.as_bytes());
    (canonical, hex_encode(&digest))
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

use sha2::Digest;

pub fn solve(evidence: &[Evidence], settings: &Settings) -> Result<RunOutput, String> {
    let model = Model::build(evidence, settings)?;
    let (feasible, conflicts) = model.feasibility(settings);
    let (canonical, run_id) = canonical_input(evidence, settings);

    if !feasible {
        let minimum = Model::minimum_conflict_set(evidence, settings);
        let record = json!({
            "run_id": run_id,
            "feasible": false,
            "conflicts": conflicts,
            "minimum_conflict_set": minimum,
            "input": serde_json::from_str::<Value>(&canonical).unwrap(),
            "quantiles": settings.quantiles,
            "created_at": crate::time::now_iso(),
        });
        return Ok(RunOutput {
            feasible: false,
            run_id,
            record,
            nodes: Vec::new(),
            conflicts,
            minimum_conflict_set: minimum,
        });
    }

    let anchors = anchor_ages(&model, settings);
    let draws = settings.draws.max(1) as usize;
    let mut accepted: Vec<Vec<f64>> = Vec::with_capacity(draws);
    let mut attempts = 0usize;
    let max_attempts = draws.saturating_mul(20).max(400);

    while accepted.len() < draws && attempts < max_attempts {
        if let Some(ages) = draw_once(&model, settings, &anchors, attempts as u32) {
            accepted.push(ages);
        }
        attempts += 1;
    }
    if accepted.len() < draws {
        return Err(format!(
            "可行域过于狭窄：仅生成 {} / {} 条样本，请放宽硬约束或调整种子",
            accepted.len(),
            draws
        ));
    }

    let mut node_results = Vec::new();
    for (i, node) in model.nodes.iter().enumerate() {
        let mut values: Vec<f64> = accepted.iter().map(|a| a[i]).collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let quantiles = settings
            .quantiles
            .iter()
            .map(|p| quantile(&values, *p))
            .collect();
        let median = quantile(&values, 0.5);
        let soft_residual = node
            .soft_mean
            .map(|mean| (median - mean) / node.soft_sigma.unwrap_or(1.0));
        let in_hard = median >= node.hard_lo - EPS && median <= node.hard_hi + EPS;
        node_results.push(NodeResult {
            key: node.key.clone(),
            depth: node.depth,
            median,
            quantiles,
            soft_residual_sigma: soft_residual,
            in_hard_box: in_hard,
            label: node.label.clone(),
        });
    }

    let mut edge_results = Vec::new();
    for edge in &model.edges {
        let mut durations: Vec<f64> = accepted
            .iter()
            .map(|ages| ages[edge.to] - ages[edge.from])
            .collect();
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let duration = quantile(&durations, 0.5);
        let rate = if edge.is_gap {
            f64::NAN
        } else {
            duration / edge.normal_thickness
        };
        edge_results.push(EdgeResult {
            key: edge.key.clone(),
            depth_lo: edge.depth_lo,
            depth_hi: edge.depth_hi,
            is_gap: edge.is_gap,
            gap_id: edge.gap_id.clone(),
            median_duration: duration,
            median_rate: rate,
        });
    }

    let violations: Vec<Value> = node_results
        .iter()
        .filter(|n| {
            n.soft_residual_sigma
                .map(|z| z.abs() > 2.5)
                .unwrap_or(false)
        })
        .map(|n| {
            json!({
                "node_key": n.key,
                "depth": n.depth,
                "median": n.median,
                "residual_sigma": n.soft_residual_sigma,
                "label": n.label,
            })
        })
        .collect();

    let record = json!({
        "run_id": run_id,
        "feasible": true,
        "input": serde_json::from_str::<Value>(&canonical).unwrap(),
        "quantiles": settings.quantiles,
        "nodes": node_results.iter().map(|n| json!({
            "key": n.key,
            "depth": n.depth,
            "median": n.median,
            "quantiles": n.quantiles,
            "soft_residual_sigma": n.soft_residual_sigma,
            "in_hard_box": n.in_hard_box,
            "label": n.label,
        })).collect::<Vec<_>>(),
        "edges": edge_results.iter().map(|e| json!({
            "key": e.key,
            "depth_lo": e.depth_lo,
            "depth_hi": e.depth_hi,
            "is_gap": e.is_gap,
            "gap_id": e.gap_id,
            "median_duration": e.median_duration,
            "median_rate": e.median_rate,
        })).collect::<Vec<_>>(),
        "soft_violations": violations,
        "draws": draws,
        "attempts": attempts,
        "created_at": crate::time::now_iso(),
    });

    Ok(RunOutput {
        feasible: true,
        run_id,
        record,
        nodes: node_results,
        conflicts: Vec::new(),
        minimum_conflict_set: Vec::new(),
    })
}

/// Nodes whose age quantiles depend on a specific gap's geometry.
/// Anchors hold independent draws; between two anchors only material
/// allocation fractions change when the gap boundary moves, so precisely
/// the non-anchor nodes in that inter-anchor span become stale.
pub fn stale_nodes_for_gap_move(
    evidence: &[Evidence],
    settings: &Settings,
    gap_id: &str,
    new_start: f64,
    new_end: f64,
) -> Result<Vec<String>, String> {
    let model_before = Model::build(evidence, settings)?;
    let gap = evidence
        .iter()
        .find(|e| e.id == gap_id)
        .ok_or_else(|| format!("缺芯 {} 不存在", gap_id))?;
    let old_start = gap.depth;
    let old_end = gap.gap_end.unwrap_or(old_start);
    let affected_top = old_start.min(new_start);
    let affected_bottom = old_end.max(new_end);

    let anchor_depths: Vec<f64> = model_before
        .nodes
        .iter()
        .filter(|n| n.anchor)
        .map(|n| n.depth)
        .collect();
    let above = anchor_depths
        .iter()
        .cloned()
        .filter(|d| *d <= affected_top + EPS)
        .fold(f64::NEG_INFINITY, f64::max);
    let below = anchor_depths
        .iter()
        .cloned()
        .find(|d| *d >= affected_bottom - EPS)
        .unwrap_or(f64::INFINITY);

    Ok(model_before
        .nodes
        .iter()
        .filter(|n| {
            let depth = n.depth;
            depth > above + EPS
                && depth < below - EPS
                && depth >= affected_top - EPS
                && depth <= affected_bottom + EPS
                && !n.anchor
        })
        .map(|n| n.key.clone())
        .collect())
}

/// All derived node keys, in depth order.
pub fn node_keys(evidence: &[Evidence], settings: &Settings) -> Result<Vec<String>, String> {
    let model = Model::build(evidence, settings)?;
    Ok(model.nodes.iter().map(|n| n.key.clone()).collect())
}
