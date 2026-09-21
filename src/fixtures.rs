//! 固定演示数据（验收场景）：
//! - 两个可交换软锦标 V15/V20（火山灰，年龄归属可对调）；
//! - 一对互相冲突的硬年代区间 I25 / C25B（默认排除 C25B，发布冲突版本时启用）；
//! - 一段缺芯 S3（21.8–22.4m，最小 30 年，严禁零年填补）。

use crate::model::*;

pub const DEFAULT_SEED: u64 = 20260921;
pub const DEFAULT_DRAWS: usize = 400;

pub fn seed_segments() -> Vec<Segment> {
    vec![
        Segment {
            id: "S1".into(),
            top: 0.0,
            bottom: 12.0,
            present: true,
            gap_min_years: None,
            note: "顶层完整芯段（含季节层计数）".into(),
            version: 1,
        },
        Segment {
            id: "S2".into(),
            top: 12.0,
            bottom: 21.8,
            present: true,
            gap_min_years: None,
            note: "中部完整芯段（火山灰 + 同位素）".into(),
            version: 1,
        },
        Segment {
            id: "S3".into(),
            top: 21.8,
            bottom: 22.4,
            present: false,
            gap_min_years: Some(30.0),
            note: "缺失芯段：边界最小年数 30".into(),
            version: 1,
        },
        Segment {
            id: "S4".into(),
            top: 22.4,
            bottom: 30.0,
            present: true,
            gap_min_years: None,
            note: "深层完整芯段".into(),
            version: 1,
        },
    ]
}

pub fn seed_markers() -> Vec<Marker> {
    vec![
        Marker {
            id: "A0".into(),
            depth: 0.0,
            kind: "anchor".into(),
            hard: true,
            lo: 0.0,
            hi: 0.0,
            mean: Some(0.0),
            sd: None,
            alt_means: vec![],
            exchange_group: None,
            weight: 1000.0,
            excluded: false,
            note: "表层锚点：年龄 0".into(),
            version: 1,
        },
        Marker {
            id: "L9".into(),
            depth: 9.0,
            kind: "layer".into(),
            hard: true,
            lo: 38.0,
            hi: 52.0,
            mean: Some(45.0),
            sd: Some(2.0),
            alt_means: vec![43.0, 45.0, 47.0],
            exchange_group: None,
            weight: 4.0,
            excluded: false,
            note: "季节层计数：9m 处累计层数，保留 43/45/47 三个局部方案".into(),
            version: 1,
        },
        Marker {
            id: "V15".into(),
            depth: 15.0,
            kind: "volcano".into(),
            hard: false,
            lo: 60.0,
            hi: 90.0,
            mean: Some(72.0),
            sd: Some(5.0),
            alt_means: vec![],
            exchange_group: Some("EX-V".into()),
            weight: 3.0,
            excluded: false,
            note: "火山灰锦标（可交换，默认与 V20 按深度正序）".into(),
            version: 1,
        },
        Marker {
            id: "V20".into(),
            depth: 20.0,
            kind: "volcano".into(),
            hard: false,
            lo: 88.0,
            hi: 120.0,
            mean: Some(104.0),
            sd: Some(5.0),
            alt_means: vec![],
            exchange_group: Some("EX-V".into()),
            weight: 3.0,
            excluded: false,
            note: "火山灰锦标（可交换，与 V15 的年龄归属可对调）".into(),
            version: 1,
        },
        Marker {
            id: "I24".into(),
            depth: 24.0,
            kind: "isotope".into(),
            hard: true,
            lo: 150.0,
            hi: 160.0,
            mean: Some(155.0),
            sd: Some(2.0),
            alt_means: vec![],
            exchange_group: None,
            weight: 4.0,
            excluded: false,
            note: "同位素对齐点（缺芯之下）".into(),
            version: 1,
        },
        Marker {
            id: "I28".into(),
            depth: 28.0,
            kind: "isotope".into(),
            hard: true,
            lo: 180.0,
            hi: 190.0,
            mean: Some(185.0),
            sd: Some(2.0),
            alt_means: vec![],
            exchange_group: None,
            weight: 4.0,
            excluded: false,
            note: "同位素对齐点（深部）".into(),
            version: 1,
        },
        // 冲突演示用：与 I28 在同深度、区间无交集；默认排除，启用后求解必须拒绝并报最小冲突集。
        Marker {
            id: "C25B".into(),
            depth: 28.0,
            kind: "isotope".into(),
            hard: true,
            lo: 150.0,
            hi: 165.0,
            mean: None,
            sd: None,
            alt_means: vec![],
            exchange_group: None,
            weight: 0.0,
            excluded: true,
            note: "冲突版本：与 I28[180,190] 在 28m 无交集（验收用，默认排除）".into(),
            version: 1,
        },
    ]
}

pub fn seed_exchanges() -> Vec<ExchangeState> {
    vec![ExchangeState {
        group: "EX-V".into(),
        swapped: false,
    }]
}

pub fn seed_input() -> SolveInput {
    SolveInput {
        seed: DEFAULT_SEED,
        draws: DEFAULT_DRAWS,
        solver_version: SOLVER_VERSION,
        segments: seed_segments(),
        markers: seed_markers(),
        exchanges: seed_exchanges(),
    }
}
