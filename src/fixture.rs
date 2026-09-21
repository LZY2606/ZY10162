//! Fixed acceptance fixtures. The default workspace is feasible; an
//! additional disabled hard interval can be enabled to demonstrate the
//! minimum conflict set.

use crate::model::{Evidence, EvidenceKind, Settings};
use rusqlite::Connection;

fn evidence(
    id: &str,
    segment: &str,
    kind: EvidenceKind,
    depth: f64,
    enabled: bool,
    label: &str,
) -> Evidence {
    Evidence {
        id: id.to_string(),
        segment: segment.to_string(),
        kind,
        depth,
        enabled,
        mean: None,
        sigma: None,
        age_lo: None,
        age_hi: None,
        gap_end: None,
        start_depth: None,
        gap_mean: None,
        gap_sigma: None,
        label: label.to_string(),
    }
}

pub fn default_evidence() -> Vec<Evidence> {
    let mut out = Vec::new();

    // Seasonal layer counts (years accumulated from the cited start depth).
    let mut layer_1 = evidence(
        "layer-A",
        "A",
        EvidenceKind::Layer,
        70.0,
        true,
        "A/B段季节层计数",
    );
    layer_1.mean = Some(440.0);
    layer_1.sigma = Some(0.0);
    layer_1.start_depth = Some(0.0);
    out.push(layer_1);

    let mut layer_2 = evidence(
        "layer-C",
        "C",
        EvidenceKind::Layer,
        120.0,
        true,
        "C段季节层计数",
    );
    layer_2.mean = Some(200.0);
    layer_2.sigma = Some(0.0);
    layer_2.start_depth = Some(82.6);
    out.push(layer_2);

    // Two exchangeable volcanic ash soft tournaments. Their distribution
    // parameters can be swapped, edited or temporarily excluded without
    // changing the hard chronology.
    let mut ash_1 = evidence("ash-1", "A", EvidenceKind::Ash, 20.0, true, "火山灰锦标 I");
    ash_1.mean = Some(132.0);
    ash_1.sigma = Some(18.0);
    out.push(ash_1);

    let mut ash_2 = evidence("ash-2", "B", EvidenceKind::Ash, 35.0, true, "火山灰锦标 II");
    ash_2.mean = Some(232.0);
    ash_2.sigma = Some(22.0);
    out.push(ash_2);

    let mut isotope = evidence(
        "isotope-1",
        "C",
        EvidenceKind::Isotope,
        90.0,
        true,
        "同位素对齐点",
    );
    isotope.mean = Some(575.0);
    isotope.sigma = Some(24.0);
    out.push(isotope);

    // Hard stratigraphic age windows (years BP).
    let mut hard_1 = evidence(
        "hard-A",
        "A",
        EvidenceKind::Hard,
        50.0,
        true,
        "硬年代区间 H_A",
    );
    hard_1.age_lo = Some(300.0);
    hard_1.age_hi = Some(340.0);
    out.push(hard_1);

    let mut hard_2 = evidence(
        "hard-C",
        "C",
        EvidenceKind::Hard,
        100.0,
        true,
        "硬年代区间 H_C",
    );
    hard_2.age_lo = Some(630.0);
    hard_2.age_hi = Some(690.0);
    out.push(hard_2);

    // Missing core section: it must carry a positive elapsed duration.
    let mut gap = evidence("gap-BC", "B->C", EvidenceKind::Gap, 70.0, true, "缺芯 BC");
    gap.gap_end = Some(82.6);
    gap.gap_mean = Some(42.0);
    gap.gap_sigma = Some(10.0);
    out.push(gap);

    // Disabled conflict fixture: same depth as hard-A with a disjoint box.
    let mut hard_conflict = evidence(
        "hard-X",
        "A",
        EvidenceKind::Hard,
        50.0,
        false,
        "冲突硬区间 H_X(停用)",
    );
    hard_conflict.age_lo = Some(346.0);
    hard_conflict.age_hi = Some(380.0);
    out.push(hard_conflict);

    out
}

pub fn default_settings() -> Settings {
    Settings::default()
}

pub(crate) fn load_defaults(conn: &mut Connection) -> rusqlite::Result<()> {
    let settings = default_settings();
    let payload = serde_json::to_string(&settings).unwrap();
    conn.execute(
        "INSERT INTO settings(id, payload) VALUES (1, ?1)",
        rusqlite::params![payload],
    )?;
    for ev in default_evidence() {
        crate::db::Database::upsert_on(conn, &ev)?;
    }
    Ok(())
}
