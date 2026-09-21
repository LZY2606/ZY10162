//! 寒层年尺 — 本地服务入口。
//!
//! 用法：
//!   han-ceng-nian-chi --listen 127.0.0.1:5502 [--db hanceng.sqlite]

use hanceng::api::router;
use hanceng::db::Store;
use hanceng::fixtures;
use hanceng::solver::solve;

struct Args {
    listen: String,
    db: String,
}

fn parse_args() -> Args {
    let mut listen = "127.0.0.1:5502".to_string();
    let mut db = "hanceng.sqlite".to_string();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => listen = it.next().expect("--listen 需要地址"),
            "--db" => db = it.next().expect("--db 需要路径"),
            "-h" | "--help" => {
                println!("用法: han-ceng-nian-chi --listen 127.0.0.1:5502 [--db 路径]");
                std::process::exit(0);
            }
            other => {
                eprintln!("未知参数: {}", other);
                std::process::exit(2);
            }
        }
    }
    Args { listen, db }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();

    let store = Store::open(&args.db)?;
    if !store.is_seeded()? {
        store.reset_to_fixture()?;
        // 空库初始化时生成一条不可变基线运行记录。
        let input = fixtures::seed_input();
        let result = solve(&input);
        let reason = result
            .conflict
            .as_ref()
            .map(|c| c.reason.clone())
            .unwrap_or_else(|| "可行模型（初始基线）".into());
        store.insert_run(&hanceng::db::NewRun {
            kind: "baseline",
            status: if result.feasible { "feasible" } else { "rejected" },
            reason,
            seed: input.seed,
            draws: input.draws,
            snapshot: &input,
            result_json: serde_json::to_string(&result)?,
            conflict_json: result
                .conflict
                .as_ref()
                .and_then(|c| serde_json::to_string(c).ok()),
            parent_seq: None,
            rerun_json: None,
        })?;
        println!("已在 {} 写入固定 fixture 与初始基线运行", args.db);
    }

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    println!("寒层年尺已启动: http://{}/", args.listen);
    axum::serve(listener, router(store)).await?;
    Ok(())
}
