use axum::serve;
use hanleng_nianchi::db::Database;
use hanleng_nianchi::web::router;
use std::net::SocketAddr;
use tokio::net::TcpListener;

struct Args {
    listen: String,
    database: String,
    reset: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        listen: "127.0.0.1:5502".to_string(),
        database: "data/hanleng-nianchi.sqlite3".to_string(),
        reset: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--listen" => args.listen = iter.next().unwrap_or_else(|| usage("--listen 需要地址")),
            "--db" => args.database = iter.next().unwrap_or_else(|| usage("--db 需要路径")),
            "--reset" => args.reset = true,
            "--help" | "-h" => usage("寒层年尺本地服务"),
            other => {
                eprintln!("未知参数 {}", other);
                usage("支持 --listen ADDR --db PATH [--reset]");
            }
        }
    }
    args
}

fn usage(msg: &str) -> ! {
    eprintln!("{}", msg);
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();
    if let Some(parent) = std::path::Path::new(&args.database).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut db = Database::open(&args.database)?;
    if args.reset {
        db.clear()?;
        Database::load_fixture(&mut db)?;
    }
    let app = router(db);
    let address: SocketAddr = args.listen.parse()?;
    let listener = TcpListener::bind(address).await?;
    println!("寒层年尺已启动: http://{}", address);
    serve(listener, app).await?;
    Ok(())
}
