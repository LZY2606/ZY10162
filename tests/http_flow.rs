use hanleng_nianchi::db::Database;
use hanleng_nianchi::web::router;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn spawn_server() -> (String, std::thread::JoinHandle<()>) {
    let path = format!("/tmp/hanleng-http-{}.sqlite3", std::process::id());
    std::fs::remove_file(&path).ok();
    let db = Database::open(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<u16>();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            axum::serve(listener, router(db)).await.unwrap();
        });
    });
    let port = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    (format!("127.0.0.1:{}", port), handle)
}

fn request(address: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
        method, path
    );
    if let Some(body) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    req.push_str("\r\n");
    if let Some(body) = body {
        req.push_str(body);
    }
    stream.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status: u16 = raw
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn jget<'a>(value: &'a serde_json::Value, pointer: &str) -> &'a serde_json::Value {
    value.pointer(pointer).unwrap()
}

#[test]
fn full_http_workflow() {
    let (address, _handle) = spawn_server();

    let (status, html) = request(&address, "GET", "/", None);
    assert_eq!(status, 200);
    assert!(html.contains("寒层年尺"));

    let (status, workspace) = request(&address, "GET", "/api/workspace", None);
    assert_eq!(status, 200);
    let workspace: serde_json::Value = serde_json::from_str(&workspace).unwrap();
    assert!(workspace["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == "ash-1"));

    // Feasible solve.
    let (status, run) = request(&address, "POST", "/api/solve", Some("{}"));
    assert_eq!(status, 200);
    let run: serde_json::Value = serde_json::from_str(&run).unwrap();
    assert_eq!(run["feasible"], true);
    let run_id = run["run_id"].as_str().unwrap().to_string();

    // Enable the conflicting hard interval and solve again.
    let body = serde_json::json!({"enabled": true}).to_string();
    let (status, _) = request(
        &address,
        "POST",
        "/api/evidence/hard-X/enabled",
        Some(&body),
    );
    assert_eq!(status, 200);
    let (status, conflict) = request(&address, "POST", "/api/solve", Some("{}"));
    assert_eq!(status, 200);
    let conflict: serde_json::Value = serde_json::from_str(&conflict).unwrap();
    assert_eq!(conflict["feasible"], false);
    let mus = conflict["minimum_conflict_set"].as_array().unwrap();
    let mus: Vec<&str> = mus.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(mus, vec!["hard-A", "hard-X"]);

    // Disable it, adjust gap and inspect precise invalidation.
    let body = serde_json::json!({"enabled": false}).to_string();
    request(
        &address,
        "POST",
        "/api/evidence/hard-X/enabled",
        Some(&body),
    );
    let body = serde_json::json!({"start": 70.0, "end": 83.6}).to_string();
    let (status, moved) = request(&address, "POST", "/api/evidence/gap-BC/gap", Some(&body));
    assert_eq!(status, 200);
    let moved: serde_json::Value = serde_json::from_str(&moved).unwrap();
    let stale = moved["invalidated_nodes"].as_array().unwrap();
    assert!(stale.iter().all(|v| v.as_str().unwrap() != "hard-A"));
    assert!(stale.iter().any(|v| {
        let key = v.as_str().unwrap();
        key == "gap-end:gap-BC" || key.starts_with("n00")
    }));

    // Run export and compare endpoints are reachable and structured.
    let (status, exported) = request(&address, "GET", &format!("/api/runs/{}", run_id), None);
    assert_eq!(status, 200);
    let exported: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(exported["run_id"].as_str().unwrap(), run_id);
    let compare =
        serde_json::json!({"left": run_id, "right": run_id, "threshold": 1.0}).to_string();
    let (status, diff) = request(&address, "POST", "/api/runs/compare", Some(&compare));
    assert_eq!(status, 200);
    let diff: serde_json::Value = serde_json::from_str(&diff).unwrap();
    assert!(jget(&diff, "/diff/segments").as_array().unwrap().is_empty());

    // Full-bundle export and replace import stay verifiable.
    let (_, bundle) = request(&address, "GET", "/api/export", None);
    let bundle_value: serde_json::Value = serde_json::from_str(&bundle).unwrap();
    let import = serde_json::json!({"mode": "replace", "bundle": bundle_value}).to_string();
    let (status, report) = request(&address, "POST", "/api/import", Some(&import));
    assert_eq!(status, 200);
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert!(report["runs_added"].as_i64().unwrap() >= 1);
}
