//! backend-proxy を起動する。
//!
//! - `DATABASE_URL`: MySQL（`mysql://user:pass@mysql:3306/shop`）。接続を持つのは proxy だけ。
//! - `PAYMENT_URL`: 決済サービス（`http://payment:8080`）。
//! - `LISTEN`: gRPC で待つ場所（既定 `0.0.0.0:50051`）。
use std::time::Duration;

use shop_proxy::ShopService;
use shop_proxy::payment::PaymentClient;
use shop_proxy_api::shop_server::ShopServer;
use sqlx::mysql::MySqlPoolOptions;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

#[tokio::main]
async fn main() {
    let listen = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:50051".to_owned());
    let addr = listen.parse().unwrap_or_else(|e| panic!("bad LISTEN {listen}: {e}"));
    let url = env("DATABASE_URL");
    // DB が上がるまで待つ（compose で同時に起動するため）
    let pool = loop {
        match MySqlPoolOptions::new().max_connections(10).connect(&url).await {
            Ok(p) => break p,
            Err(e) => {
                eprintln!("proxy: waiting for the database: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    let service = ShopService::new(pool, PaymentClient::new(env("PAYMENT_URL")));
    eprintln!("proxy: listening on {listen}");
    tonic::transport::Server::builder()
        .add_service(ShopServer::new(service))
        .serve_with_shutdown(addr, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}
