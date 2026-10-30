//! backend-proxy を起動する。
//!
//! - `DATABASE_URL`: MySQL（`mysql://user:pass@mysql:3306/shop`）。接続を持つのは proxy だけ。
//! - `PAYMENT_URL`: 決済サービス（`http://payment:8080`）。
//! - `SESSION_SECRET`: セッションの署名の鍵（32 バイト以上。なければ起動しない）。持つのは proxy だけ。
//! - `SESSION_TTL`: セッションの有効な秒数（既定 86400 = 1 日）。
//! - `RECONCILE_AFTER`: 決着しないまま何秒たった pending の注文を、決済サービスと突き合わせて片付けるか
//!   （既定 600。1 分ごとに確かめる）。
//! - `LISTEN`: gRPC で待つ場所（既定 `0.0.0.0:50051`）。
use std::time::Duration;

use shop_proxy::ShopService;
use shop_proxy::auth::{Passwords, Sessions};
use shop_proxy::payment::PaymentClient;
use shop_proxy::queries;
use shop_proxy_api::shop_server::ShopServer;
use sqlx::mysql::MySqlPoolOptions;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[tokio::main]
async fn main() {
    let listen = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:50051".to_owned());
    let addr = listen.parse().unwrap_or_else(|e| panic!("bad LISTEN {listen}: {e}"));
    let ttl =
        std::env::var("SESSION_TTL").map_or(86_400, |v| v.parse().unwrap_or_else(|e| panic!("bad SESSION_TTL: {e}")));
    let sessions = Sessions::new(env("SESSION_SECRET").as_bytes(), ttl).unwrap_or_else(|e| panic!("{e}"));
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let passwords = Passwords::new(cpus, Duration::from_secs(2));
    let url = env("DATABASE_URL");
    // DB が上がるまで待つ（compose で同時に起動するため）
    let pool = loop {
        let options =
            MySqlPoolOptions::new().max_connections(10).acquire_timeout(Duration::from_secs(3)).after_connect(
                |conn, _| Box::pin(async move { sqlx::query(queries::SESSION_SETUP).execute(conn).await.map(|_| ()) }),
            );
        match options.connect(&url).await {
            Ok(p) => break p,
            Err(e) => {
                eprintln!("proxy: waiting for the database: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    let payment = PaymentClient::new(env("PAYMENT_URL"), Duration::from_secs(5));
    let service = ShopService::new(pool, payment, sessions, passwords);
    let after: u64 = std::env::var("RECONCILE_AFTER")
        .map_or(600, |v| v.parse().unwrap_or_else(|e| panic!("bad RECONCILE_AFTER: {e}")));
    let reconciler = service.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            match reconciler.reconcile(after).await {
                Ok(0) => {}
                Ok(n) => eprintln!("proxy: reconciled {n} pending orders"),
                Err(e) => eprintln!("proxy: reconcile failed: {e}"),
            }
        }
    });
    eprintln!("proxy: listening on {listen}");
    tonic::transport::Server::builder()
        .timeout(Duration::from_secs(15))
        .concurrency_limit_per_connection(64)
        .add_service(ShopServer::new(service))
        .serve_with_shutdown(addr, shutdown())
        .await
        .expect("server");
}
