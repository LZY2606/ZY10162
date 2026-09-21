#![allow(dead_code)]
use hanceng::api::router;
use hanceng::db::Store;
use std::net::SocketAddr;

pub async fn spawn_server() -> (String, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let rand_part = nanos.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) & 0xffff_ffff;
    let unique = format!(
        "hanceng-test-{}-{}-{}.sqlite",
        std::process::id(), seq, rand_part
    );
    let path = dir.join(unique);
    let _ = std::fs::remove_file(&path);
    let store = Store::open(path.to_str().unwrap()).unwrap();
    store.reset_to_fixture().unwrap();
    let app = router(store);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{}", addr), path)
}

pub async fn get_json(client: &reqwest::Client, url: String) -> serde_json::Value {
    let res = client.get(url).send().await.unwrap();
    assert!(res.status().is_success());
    res.json().await.unwrap()
}

pub async fn post_json(
    client: &reqwest::Client,
    url: String,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let res = client.post(url).json(&body).send().await.unwrap();
    let status = res.status();
    let text = res.text().await.unwrap();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::json!({"raw": text}));
    (status, json)
}
