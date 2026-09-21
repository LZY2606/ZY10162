mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn full_acceptance_flow_over_http() {
    let (base, _dbpath) = spawn_server().await;
    let client = reqwest::Client::new();

    // 首页必须包含标题
    let html = client
        .get(format!("{}/", base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("寒层年尺"));

    // 1) 可行模型
    let (st, body) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 300}),
    )
    .await;
    assert_eq!(st, 200, "{}", body);
    let seq_a = body["seq"].as_i64().unwrap();
    assert_eq!(body["result"]["feasible"], true);
    let nodes = body["result"]["nodes"].as_array().unwrap();
    // 中位数随深度不减
    let mut prev = f64::NEG_INFINITY;
    for n in nodes {
        let med = n["median"].as_f64().unwrap();
        assert!(med + 1e-12 >= prev);
        prev = med;
        let lo = n["q05"].as_f64().unwrap();
        let hi = n["q95"].as_f64().unwrap();
        assert!(lo <= med && med <= hi);
    }
    // 缺芯边恰好为最小年数 30
    let gap = body["result"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["is_gap"] == true)
        .unwrap()
        .clone();
    assert_eq!(gap["rate_median"], 30.0);
    assert_eq!(gap["gap_segment"], "S3");

    // 2) 固定种子重放：完全相同分位数
    let (_, body2) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 300}),
    )
    .await;
    assert_eq!(body["result"]["nodes"], body2["result"]["nodes"]);
    let seq_b = body2["seq"].as_i64().unwrap();
    assert_ne!(seq_a, seq_b);

    // 3) 交换可交换软锦标（对调 V15/V20 的分布归属）
    let (st, _) = post_json(
        &client,
        format!("{}/api/exchanges", base),
        json!({"group": "EX-V", "swapped": true}),
    )
    .await;
    assert_eq!(st, 200);
    let (_, swapped) = post_json(
        &client,
        format!("{}/api/rerun", base),
        json!({"base_seq": seq_b}),
    )
    .await;
    assert_eq!(swapped["report"]["result"]["feasible"], true);
    let seq_swap = swapped["seq"].as_i64().unwrap();

    // 差异定位：交换只影响 V15/V20 所在深度段（20m 处差异应大于 0，0m 锚点为 0）
    let diff = client
        .get(format!("{}/api/runs/{}/diff/{}", base, seq_b, seq_swap))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let at0 = diff["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["depth"] == 0.0)
        .unwrap();
    assert_eq!(at0["median_delta"], 0.0);
    let near = diff["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| {
            let d = n["depth"].as_f64().unwrap();
            (12.0..=21.8).contains(&d)
        })
        .map(|n| n["median_delta"].as_f64().unwrap())
        .fold(0.0, f64::max);
    assert!(near > 0.0, "交换影响应定位到 12-21.8m");

    // 恢复交换状态
    post_json(
        &client,
        format!("{}/api/exchanges", base),
        json!({"group": "EX-V", "swapped": false}),
    )
    .await;

    // 4) 启用互相冲突的硬区间 C25B → 拒绝发布 + 最小冲突集
    let (st, _) = post_json(
        &client,
        format!("{}/api/markers/C25B", base),
        json!({"excluded": false}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, rejected) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 300}),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(rejected["result"]["feasible"], false);
    let c = &rejected["result"]["conflict"];
    assert_eq!(c["minimal"], true);
    let ids: Vec<String> = c["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"I28".to_string()));
    assert!(ids.contains(&"C25B".to_string()));
    // 被拒绝也必须留下不可变记录
    let runs = get_json(&client, format!("{}/api/runs", base)).await;
    let last = runs.as_array().unwrap().last().unwrap();
    assert_eq!(last["status"], "rejected");

    // 5) 恢复冲突锦标后重新可行
    post_json(
        &client,
        format!("{}/api/markers/C25B", base),
        json!({"excluded": true}),
    )
    .await;
    let (_, ok) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 300}),
    )
    .await;
    assert_eq!(ok["result"]["feasible"], true);
}

#[tokio::test]
async fn touching_hard_intervals_are_accepted_and_zero_gap_rejected() {
    let (base, _) = spawn_server().await;
    let client = reqwest::Client::new();

    // 把 C25B 改成与 I28[180,190] 恰好相触：[178,180]
    let (st, body) = post_json(
        &client,
        format!("{}/api/markers/C25B", base),
        json!({"excluded": false, "lo": 178.0, "hi": 180.0}),
    )
    .await;
    assert_eq!(st, 200, "{}", body);
    let (_, r) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 200}),
    )
    .await;
    assert_eq!(r["result"]["feasible"], true, "相触区间必须可行: {}", r);

    // 缺芯零年必须被拒绝（先恢复 C25B 排除避免干扰）
    post_json(
        &client,
        format!("{}/api/markers/C25B", base),
        json!({"excluded": true}),
    )
    .await;
    let (st, err) = post_json(
        &client,
        format!("{}/api/segments/S3", base),
        json!({"gap_min_years": 0.0}),
    )
    .await;
    assert_eq!(st, 400);
    assert!(err["error"].as_str().unwrap().contains("零年"));
}
