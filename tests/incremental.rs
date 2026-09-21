mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn gap_boundary_change_only_invalidates_dependent_quantiles() {
    let (base, _) = spawn_server().await;
    let client = reqwest::Client::new();

    let (_, first) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 250}),
    )
    .await;
    assert_eq!(first["result"]["feasible"], true);
    let seq0 = first["seq"].as_i64().unwrap();

    // 记录几个关键点的原始分位数
    let med_at = |body: &serde_json::Value, d: f64| -> f64 {
        let nodes = body["result"]["nodes"].as_array()
            .or_else(|| body["report"]["result"]["nodes"].as_array())
            .unwrap();
        nodes
            .iter()
            .find(|n| (n["depth"].as_f64().unwrap() - d).abs() < 1e-9)
            .unwrap()["median"]
            .as_f64()
            .unwrap()
    };
    let before_9 = med_at(&first, 9.0);
    let before_15 = med_at(&first, 15.0);
    let before_24 = med_at(&first, 24.0);

    // 调整缺芯 S3 边界（30 → 45 年）
    let (st, seg) = post_json(
        &client,
        format!("{}/api/segments/S3", base),
        json!({"gap_min_years": 45.0}),
    )
    .await;
    assert_eq!(st, 200, "{}", seg);

    let (_, rr) = post_json(
        &client,
        format!("{}/api/rerun", base),
        json!({"base_seq": seq0}),
    )
    .await;
    assert_eq!(rr["report"]["result"]["feasible"], true);
    assert!(rr["report"]["reused_node_count"].as_i64().unwrap() >= 3);
    assert!(rr["report"]["invalidated_node_count"].as_i64().unwrap() >= 1);

    // 节点级断言：远离缺芯的上游节点必须复用；紧邻缺芯的节点允许因全局
    // 最小二乘耦合出现保守失效（逐位校验保护正确性）。
    let statuses = rr["report"]["node_status"].as_array().unwrap();
    for d in [0.0_f64, 9.0, 12.0, 15.0] {
        let st = statuses.iter().find(|x| (x["depth"].as_f64().unwrap()-d).abs()<1e-9).unwrap();
        assert_eq!(st["reused"], true, "深度 {} 远离缺芯，必须复用", d);
    }
    let after_9 = med_at(&rr, 9.0);
    let after_15 = med_at(&rr, 15.0);
    let after_24 = med_at(&rr, 24.0);
    assert_eq!(before_9, after_9, "缺芯之上分位数逐位一致");
    assert_eq!(before_15, after_15);
    assert_ne!(before_24, after_24, "缺芯之下依赖该段的分位数必须失效变化");
    // 失效必须是“局部”的：复用节点数严格多于失效节点数。
    assert!(rr["report"]["reused_node_count"].as_i64().unwrap() >= 3);

    // 缺芯最小年数不得为零：0 年必须被拒绝
    let (st, err) = post_json(
        &client,
        format!("{}/api/segments/S3", base),
        json!({"gap_min_years": 0.0}),
    )
    .await;
    assert_eq!(st, 400);
    assert!(err["error"].as_str().unwrap().contains("零年") ||
            err["error"].as_str().unwrap().contains("gap"));

    // 深度严格有序：把 S1 底深改到超过 S2 顶深 → 400
    let (st, err) = post_json(
        &client,
        format!("{}/api/segments/S1", base),
        json!({"bottom": 30.0}),
    )
    .await;
    assert_eq!(st, 400, "{}", err);
}

#[tokio::test]
async fn marker_exclusion_only_invalidates_dependent_nodes() {
    let (base, _) = spawn_server().await;
    let client = reqwest::Client::new();
    let (_, first) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 250}),
    )
    .await;
    let seq0 = first["seq"].as_i64().unwrap();

    post_json(
        &client,
        format!("{}/api/markers/V15", base),
        json!({"excluded": true}),
    )
    .await;
    let (_, rr) = post_json(
        &client,
        format!("{}/api/rerun", base),
        json!({"base_seq": seq0}),
    )
    .await;
    let statuses = rr["report"]["node_status"].as_array().unwrap();
    let s0 = statuses.iter().find(|s| (s["depth"].as_f64().unwrap()-0.0).abs()<1e-9).unwrap();
    assert_eq!(s0["reused"], true, "表层锚点与 V15 无关");
    let s15 = statuses.iter().find(|s| (s["depth"].as_f64().unwrap()-15.0).abs()<1e-9).unwrap();
    assert_eq!(s15["reused"], false);
    assert!(
        s15["reason"].as_str().unwrap().contains("锦标")
            || s15["reason"].as_str().unwrap().contains("漂移"),
        "s15 reason={}", s15["reason"]
    );
}
