//! proxy を本物の MySQL（testdata のスキーマ）と模擬の決済サービスにつなぎ、gRPC で呼ぶ。
//!
//! `DATABASE_URL` は `make test` が渡す（MySQL のコンテナを同じ docker ネットワークに立てる）。
//! 決済サービスはテストの中に小さな偽物を立てる（承認は tok_visa だけ）。
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::http::StatusCode;
use shop_proxy::ShopService;
use shop_proxy::payment::PaymentClient;
use shop_proxy_api::shop_client::ShopClient;
use shop_proxy_api::shop_server::ShopServer;
use shop_proxy_api::*;
use sqlx::mysql::MySqlPoolOptions;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Code;
use tonic::transport::Channel;

async fn fake_payment() -> SocketAddr {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let app = axum::Router::new().route(
        "/charges",
        axum::routing::post(|Json(b): Json<serde_json::Value>| async move {
            if b["card_token"] == "tok_visa" {
                let id = format!("ch_test_{}", NEXT.fetch_add(1, Ordering::SeqCst));
                (StatusCode::CREATED, Json(serde_json::json!({"id": id, "status": "succeeded"})))
            } else {
                (
                    StatusCode::PAYMENT_REQUIRED,
                    Json(serde_json::json!({"status": "declined", "reason": "card declined"})),
                )
            }
        }),
    );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

async fn client() -> ShopClient<Channel> {
    let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL (run the tests with `make test`)");
    let until = Instant::now() + Duration::from_secs(180);
    let pool = loop {
        match MySqlPoolOptions::new().max_connections(4).connect(&url).await {
            Ok(p) => break p,
            Err(e) if Instant::now() > until => panic!("database: {e}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    };
    let payment = fake_payment().await;
    let service = ShopService::new(pool, PaymentClient::new(format!("http://{payment}")));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ShopServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(l))
            .await
            .unwrap()
    });
    ShopClient::connect(format!("http://{addr}")).await.unwrap()
}

/// テストごとに別の顧客を作る。
async fn new_customer(c: &mut ShopClient<Channel>, name: &str) -> u64 {
    let email = format!("{name}-{}@example.com", std::process::id());
    c.create_customer(CreateCustomerRequest {
        email,
        password_hash: "$argon2id$test".to_owned(),
        full_name: "試験 太郎".to_owned(),
        address: "東京都".to_owned(),
    })
    .await
    .unwrap()
    .into_inner()
    .id
}

fn key(n: u32) -> String {
    format!("00000000-0000-4000-8000-{:04x}{n:08x}", std::process::id() & 0xffff)
}

fn order(customer_id: u64, request_key: String, lines: &[(u64, u32)], card: &str) -> PlaceOrderRequest {
    PlaceOrderRequest {
        customer_id,
        request_key,
        lines: lines.iter().map(|&(product_id, quantity)| OrderLine { product_id, quantity }).collect(),
        card_token: card.to_owned(),
    }
}

async fn stock(c: &mut ShopClient<Channel>, id: u64) -> i32 {
    c.get_product(GetProductRequest { id }).await.unwrap().into_inner().stock
}

#[tokio::test]
async fn customers_are_registered_once_per_email() {
    let mut c = client().await;
    let id = new_customer(&mut c, "once").await;
    let email = format!("once-{}@example.com", std::process::id());
    let again = c
        .create_customer(CreateCustomerRequest {
            email: email.clone(),
            password_hash: "x".to_owned(),
            full_name: "x".to_owned(),
            address: "x".to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(again.code(), Code::AlreadyExists);
    let cred = c.find_credential(FindCredentialRequest { email }).await.unwrap().into_inner();
    assert_eq!((cred.customer_id, cred.password_hash.as_str()), (id, "$argon2id$test"));
    let missing = c.find_credential(FindCredentialRequest { email: "nobody@example.com".to_owned() }).await;
    assert_eq!(missing.unwrap_err().code(), Code::NotFound);
}

#[tokio::test]
async fn products_are_listed() {
    let mut c = client().await;
    let list = c.list_products(ListProductsRequest {}).await.unwrap().into_inner().products;
    assert!(list.iter().any(|p| p.code == "TSHIRT-001" && p.price == 2500));
}

#[tokio::test]
async fn an_order_takes_prices_from_the_database_and_is_idempotent() {
    let mut c = client().await;
    let customer = new_customer(&mut c, "idem").await;
    let before = stock(&mut c, 10).await;
    let req = order(customer, key(1), &[(10, 3)], "tok_visa");
    let first = c.place_order(req.clone()).await.unwrap().into_inner();
    let second = c.place_order(req).await.unwrap().into_inner();
    assert_eq!(first, second);
    assert_eq!(first.total, 3000);
    assert_eq!(first.status, "paid");
    assert_eq!(first.items[0].unit_price, 1000);
    assert_eq!(stock(&mut c, 10).await, before - 3);
    let mine = c.list_orders(ListOrdersRequest { customer_id: customer }).await.unwrap().into_inner().orders;
    assert_eq!(mine, vec![first]);
}

#[tokio::test]
async fn the_same_product_on_two_lines_is_reserved_together() {
    let mut c = client().await;
    let customer = new_customer(&mut c, "lines").await;
    let before = stock(&mut c, 12).await;
    let o = c.place_order(order(customer, key(2), &[(12, 2), (12, 5)], "tok_visa")).await.unwrap().into_inner();
    assert_eq!(o.items.len(), 2);
    assert_eq!(o.total, 7 * 250);
    assert_eq!(stock(&mut c, 12).await, before - 7);
}

#[tokio::test]
async fn out_of_stock_and_declined_payments_write_nothing() {
    let mut c = client().await;
    let customer = new_customer(&mut c, "fail").await;
    let e = c.place_order(order(customer, key(3), &[(4, 1)], "tok_visa")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::FailedPrecondition, "out of stock: ステッカー"));
    let before = stock(&mut c, 11).await;
    let e = c.place_order(order(customer, key(4), &[(11, 1)], "tok_declined")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::FailedPrecondition, "payment declined: card declined"));
    assert_eq!(stock(&mut c, 11).await, before);
    assert!(c.list_orders(ListOrdersRequest { customer_id: customer }).await.unwrap().into_inner().orders.is_empty());
}

#[tokio::test]
async fn bad_requests_are_rejected_before_touching_the_database() {
    let mut c = client().await;
    for (req, code) in [
        (order(1, "not-a-uuid".to_owned(), &[(1, 1)], "tok_visa"), Code::InvalidArgument),
        (order(1, key(5), &[(1, 0)], "tok_visa"), Code::InvalidArgument),
        (order(1, key(6), &[(1, 100)], "tok_visa"), Code::InvalidArgument),
        (order(1, key(7), &[], "tok_visa"), Code::InvalidArgument),
        (order(1, key(8), &[(999, 1)], "tok_visa"), Code::NotFound),
    ] {
        assert_eq!(c.place_order(req).await.unwrap_err().code(), code);
    }
}

#[tokio::test]
async fn customers_see_only_their_own_orders() {
    let mut c = client().await;
    let alice = new_customer(&mut c, "alice").await;
    let eve = new_customer(&mut c, "eve").await;
    let o = c.place_order(order(alice, key(9), &[(1, 1)], "tok_visa")).await.unwrap().into_inner();
    let e = c.get_order(GetOrderRequest { customer_id: eve, order_id: o.id }).await.unwrap_err();
    assert_eq!(e.code(), Code::NotFound);
    assert_eq!(c.get_order(GetOrderRequest { customer_id: alice, order_id: o.id }).await.unwrap().into_inner(), o);
}
