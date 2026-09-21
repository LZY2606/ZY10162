mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn export_clear_import_and_verify_reproduces_runs() {
    let (base, _) = spawn_server().await;
    let client = reqwest::Client::new();

    // 生成两条可行 + 一条被拒绝的运行记录
    let (_, r1) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 200}),
    )
    .await;
    let s1 = r1["seq"].as_i64().unwrap();
    post_json(
        &client,
        format!("{}/api/segments/S3", base),
        json!({"gap_min_years": 33.0}),
    )
    .await;
    let (_, r2) = post_json(
        &client,
        format!("{}/api/rerun", base),
        json!({"base_seq": s1}),
    )
    .await;
    assert_eq!(r2["report"]["result"]["feasible"], true);
    post_json(
        &client,
        format!("{}/api/markers/C25B", base),
        json!({"excluded": false}),
    )
    .await;
    let (_, rj) = post_json(
        &client,
        format!("{}/api/solve", base),
        json!({"seed": 20260921, "draws": 200}),
    )
    .await;
    assert_eq!(rj["result"]["feasible"], false);

    let bundle = client
        .get(format!("{}/api/export", base))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(bundle["format"], "han-ceng-nian-chi/bundle.v1");
    assert!(bundle["runs"].as_array().unwrap().len() >= 3);

    // 清空数据库
    let (st, cleared) = post_json(
        &client,
        format!("{}/api/admin/clear", base),
        json!({}),
    )
    .await;
    assert_eq!(st, 200, "{}", cleared);
    let state = get_json(&client, format!("{}/api/state", base)).await;
    assert_eq!(state["segments"].as_array().unwrap().len(), 0);

    // 导入并复核：每条记录按快照重放，逐位核对
    let (st, report) = post_json(
        &client,
        format!("{}/api/import", base),
        bundle.clone(),
    )
    .await;
    assert_eq!(st, 200, "导入复核应通过: {}", report);
    assert_eq!(report["all_ok"], true);
    assert!(report["imported_runs"].as_i64().unwrap() >= 3);
    for item in report["items"].as_array().unwrap() {
        assert_eq!(item["ok"], true, "{}", item);
    }

    // 导入后状态恢复，记录仍可查询
    let state = get_json(&client, format!("{}/api/state", base)).await;
    assert!(state["segments"].as_array().unwrap().len() >= 4);
    assert!(state["runs"].as_array().unwrap().len() >= 3);

    // 篡改一个分位数后导入必须失败（不可复现）
    let mut tampered = bundle.clone();
    let runs = tampered["runs"].as_array_mut().unwrap();
    let mut parsed: serde_json::Value =
        serde_json::from_str(runs[0]["result_json"].as_str().unwrap()).unwrap();
    parsed["nodes"][0]["median"] = json!(999.0);
    runs[0]["result_json"] = json!(serde_json::to_string(&parsed).unwrap());
    let (st, err) = post_json(
        &client,
        format!("{}/api/import", base),
        tampered,
    )
    .await;
    assert_eq!(st, 400);
    assert!(err["error"].as_str().unwrap().contains("复核失败"));

    // 复核失败不得改动数据库（原记录仍在）
    let state = get_json(&client, format!("{}/api/state", base)).await;
    assert!(state["runs"].as_array().unwrap().len() >= 3);
}
