use hanleng_nianchi::db::Database;
use hanleng_nianchi::fixture::default_evidence;
use hanleng_nianchi::model::{Evidence, EvidenceKind, Model, Settings};
use hanleng_nianchi::solver;
use serde_json::{json, Value};
use std::collections::HashMap;

fn settings() -> Settings {
    Settings::default()
}

fn find<'a>(evidence: &'a [Evidence], id: &str) -> &'a Evidence {
    evidence.iter().find(|e| e.id == id).unwrap()
}

fn find_mut<'a>(evidence: &'a mut [Evidence], id: &str) -> &'a mut Evidence {
    evidence.iter_mut().find(|e| e.id == id).unwrap()
}

#[test]
fn default_fixture_is_feasible_and_monotone() {
    let evidence = default_evidence();
    let settings = settings();
    let output = solver::solve(&evidence, &settings).expect("default fixture solves");
    assert!(output.feasible);
    let record = output.record;
    assert!(record["soft_violations"].as_array().unwrap().is_empty());
    let nodes = record["nodes"].as_array().unwrap();
    for pair in nodes.windows(2) {
        assert!(pair[1]["median"].as_f64().unwrap() >= pair[0]["median"].as_f64().unwrap() - 1e-9);
    }
    let depths: Vec<f64> = nodes.iter().map(|n| n["depth"].as_f64().unwrap()).collect();
    for pair in depths.windows(2) {
        assert!(pair[1] > pair[0]);
    }
}

#[test]
fn fixed_seed_replay_is_bit_identical() {
    let evidence = default_evidence();
    let settings = settings();
    let first = solver::solve(&evidence, &settings).unwrap().record;
    let second = solver::solve(&evidence, &settings).unwrap().record;
    assert_eq!(first["run_id"], second["run_id"]);
    assert_eq!(first["nodes"], second["nodes"]);
    assert_eq!(first["edges"], second["edges"]);
}

#[test]
fn exchangeable_soft_tournaments_can_be_toggled_or_excluded() {
    let mut evidence = default_evidence();
    let settings = settings();
    find_mut(&mut evidence, "ash-2").enabled = false;
    let output = solver::solve(&evidence, &settings).unwrap();
    assert!(output.feasible);
    let record = output.record;
    assert!(record["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["key"] == "ash-1"));

    // The two soft tournaments are exchangeable: each can carry either
    // distribution and the chronology remains feasible; hard nodes stay
    // inside their boxes in both variants.
    find_mut(&mut evidence, "ash-2").enabled = true;
    let mut swapped = evidence.clone();
    let a = (find(&swapped, "ash-1").mean, find(&swapped, "ash-1").sigma);
    let b = (find(&swapped, "ash-2").mean, find(&swapped, "ash-2").sigma);
    find_mut(&mut swapped, "ash-1").mean = b.0;
    find_mut(&mut swapped, "ash-1").sigma = b.1;
    find_mut(&mut swapped, "ash-2").mean = a.0;
    find_mut(&mut swapped, "ash-2").sigma = a.1;
    let baseline = solver::solve(&evidence, &settings).unwrap();
    let swapped_run = solver::solve(&swapped, &settings).unwrap();
    assert!(swapped_run.feasible);
    assert_ne!(baseline.run_id, swapped_run.run_id);
    let hard_median = |record: &Value, key: &str| {
        record["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["key"] == key)
            .unwrap()["median"]
            .as_f64()
            .unwrap()
    };
    // Hard anchors barely move when only soft distributions are permuted.
    for key in ["hard-A", "hard-C"] {
        assert!(
            (hard_median(&baseline.record, key) - hard_median(&swapped_run.record, key)).abs()
                < 25.0
        );
    }
}

#[test]
fn hard_conflict_is_rejected_with_minimum_set() {
    let mut evidence = default_evidence();
    let settings = settings();
    find_mut(&mut evidence, "hard-X").enabled = true;
    let model = Model::build(&evidence, &settings).unwrap();
    let (feasible, conflicts) = model.feasibility(&settings);
    assert!(!feasible);
    assert_eq!(conflicts[0].at_depth, 50.0);
    let output = solver::solve(&evidence, &settings).unwrap();
    assert!(!output.feasible);
    assert_eq!(
        output.minimum_conflict_set,
        vec!["hard-A".to_string(), "hard-X".to_string()]
    );
    assert!(output.record["conflicts"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("无交集"));

    // Removing either member restores feasibility.
    for remove in ["hard-A", "hard-X"] {
        let kept: Vec<Evidence> = evidence
            .iter()
            .filter(|e| e.id != remove)
            .cloned()
            .collect();
        assert!(solver::solve(&kept, &settings).unwrap().feasible);
    }
}

#[test]
fn touching_intervals_are_feasible_but_disjoint_are_not() {
    let settings = settings();
    let mut touching = default_evidence();
    let hx = find_mut(&mut touching, "hard-X");
    hx.enabled = true;
    hx.age_lo = Some(340.0);
    hx.age_hi = Some(360.0);
    assert!(solver::solve(&touching, &settings).unwrap().feasible);

    let mut disjoint = touching.clone();
    let hx = find_mut(&mut disjoint, "hard-X");
    hx.age_lo = Some(340.0 + 1e-4);
    let result = solver::solve(&disjoint, &settings).unwrap();
    assert!(!result.feasible);
    assert_eq!(result.minimum_conflict_set, vec!["hard-A", "hard-X"]);
}

#[test]
fn missing_core_carries_positive_duration() {
    let evidence = default_evidence();
    let settings = settings();
    let record = solver::solve(&evidence, &settings).unwrap().record;
    let gap = record["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "gap:gap-BC")
        .unwrap();
    assert!(gap["median_duration"].as_f64().unwrap() >= 1.0);
    let zero_years: Value = serde_json::json!(1e-9);
    assert!(gap["median_duration"].as_f64().unwrap() > zero_years.as_f64().unwrap());
}

#[test]
fn moving_gap_only_invalidates_dependent_quantiles() {
    let settings = settings();
    let evidence = default_evidence();
    let before = solver::solve(&evidence, &settings).unwrap().record;
    let by_key: HashMap<String, Value> = before["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["key"].as_str().unwrap().to_string(), n.clone()))
        .collect();

    // Move the lower gap boundary slightly: the B/C gap spans 70–83.6m.
    let stale =
        solver::stale_nodes_for_gap_move(&evidence, &settings, "gap-BC", 70.0, 83.6).unwrap();
    let mut moved = evidence.clone();
    let gap = find_mut(&mut moved, "gap-BC");
    gap.gap_end = Some(83.6);
    let after = solver::solve(&moved, &settings).unwrap().record;
    let after_map: HashMap<String, Value> = after["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["key"].as_str().unwrap().to_string(), n.clone()))
        .collect();

    // Stale set contains no anchor and everything stays at/inside the
    // affected span; nodes above 70 m are not even listed.
    assert!(stale
        .iter()
        .all(|key| key != "hard-A" && key != "isotope-1" && key != "hard-C"));
    for (key, node) in &by_key {
        if node["depth"].as_f64().unwrap() < 70.0 - 1e-9 {
            assert!(!stale.contains(key), "上方节点 {} 不应失效", key);
            assert_eq!(
                node["quantiles"], after_map[key]["quantiles"],
                "上方节点 {} 分位数必须逐位相同",
                key
            );
        }
    }
    // Anchor ages remain bit-identical across the boundary move.
    for key in ["ash-1", "ash-2", "hard-A", "isotope-1", "hard-C"] {
        assert_eq!(by_key[key]["quantiles"], after_map[key]["quantiles"]);
    }
    // The gap lower node and material nodes below until the isotope anchor
    // are the dependent segment.
    assert!(stale
        .iter()
        .any(|key| by_key[key]["depth"].as_f64().unwrap() > 70.0));
}

#[test]
fn sqlite_roundtrip_export_import_and_verify() {
    let tmp = temp_db_path("roundtrip");
    std::fs::remove_file(&tmp).ok();
    let db = Database::open(&tmp).unwrap();
    let evidence = db.list_evidence().unwrap();
    let settings = db.load_settings();
    let output = solver::solve(&evidence, &settings).unwrap();
    db.store_run(&output.record).unwrap();
    db.replace_quantiles(&output.record).unwrap();
    let run_id = output.run_id.clone();

    let bundle = db.export_bundle().unwrap();
    db.clear().unwrap();
    assert!(db.get_run(&run_id).unwrap().is_none());
    let report = db.import_bundle(&bundle, true).unwrap();
    assert_eq!(report.runs_added, 1);
    let restored = db.get_run(&run_id).unwrap().unwrap();
    let evidence = db.list_evidence().unwrap();
    let replay = solver::solve(&evidence, &db.load_settings()).unwrap();
    assert_eq!(replay.run_id, run_id);
    // Quantiles and medians replay bit-identically; derived residual fields
    // are recomputed floats and may differ by one ULP after JSON round-trip.
    let restored_nodes: HashMap<String, Value> = restored["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["key"].as_str().unwrap().to_string(), n.clone()))
        .collect();
    for node in replay.record["nodes"].as_array().unwrap() {
        let key = node["key"].as_str().unwrap();
        assert_eq!(restored_nodes[key]["quantiles"], node["quantiles"]);
        assert_eq!(
            restored_nodes[key]["median"].as_f64().unwrap(),
            node["median"].as_f64().unwrap()
        );
    }

    // Importing the same immutable bundle again is an idempotent no-op.
    let second = db.import_bundle(&bundle, false).unwrap();
    assert_eq!(second.runs_added, 0);
    assert_eq!(second.runs_kept, 1);
}

#[test]
fn invalid_gap_geometry_is_rejected() {
    let mut evidence = default_evidence();
    let settings = settings();
    let gap = find_mut(&mut evidence, "gap-BC");
    gap.gap_end = Some(gap.depth);
    let err = Model::build(&evidence, &settings).unwrap_err();
    assert!(err.contains("严格大于"));
}

#[test]
fn evidence_inside_gap_is_rejected() {
    let mut evidence = default_evidence();
    let settings = settings();
    find_mut(&mut evidence, "isotope-1").depth = 75.0;
    let err = Model::build(&evidence, &settings).unwrap_err();
    assert!(err.contains("缺芯内部"));
}

fn temp_db_path(name: &str) -> String {
    let pid = std::process::id();
    format!("/tmp/hanleng-test-{}-{}.sqlite3", name, pid)
}
