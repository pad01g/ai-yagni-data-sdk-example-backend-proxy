//! proxy を本物の MySQL（testdata のスキーマ）と模擬の決済サービスにつなぎ、gRPC で呼ぶ。
//!
//! `DATABASE_URL` は `make test` が渡す（MySQL のコンテナを同じ docker ネットワークに立てる）。
//! 決済サービスはテストの中に小さな偽物を立てる。本物と同じく冪等キーで結果を覚え、承認は tok_visa だけ。
//! 偽物は止めたり（届かない）、記録した後で答えを落としたり（答えが失われる）できる。
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use shop_proxy::ShopService;
use shop_proxy::auth::{Passwords, Sessions};
use shop_proxy::payment::PaymentClient;
use shop_proxy_api::shop_client::ShopClient;
use shop_proxy_api::shop_server::ShopServer;
use shop_proxy_api::*;
use sqlx::mysql::MySqlPoolOptions;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Code;
use tonic::transport::Channel;

const SECRET: &[u8] = b"a test secret that is long enough";

/// 偽の決済サービスの調子。
const UP: u8 = 0;
/// 届かない（何も記録しない）。
const DOWN: u8 = 1;
/// 記録はするが、答えが失われる。
const LOSES_ANSWERS: u8 = 2;

/// 冪等キー → (依頼, 答えの状態, 答え)
type Charges = HashMap<String, (serde_json::Value, StatusCode, serde_json::Value)>;

#[derive(Clone, Default)]
struct FakePayment {
    mode: Arc<AtomicU8>,
    charges: Arc<Mutex<Charges>>,
    calls: Arc<AtomicU64>,
}

impl FakePayment {
    fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

async fn charge(
    State(f): State<FakePayment>,
    Json(b): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    f.calls.fetch_add(1, Ordering::SeqCst);
    let mode = f.mode.load(Ordering::SeqCst);
    if mode == DOWN {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "down"})));
    }
    let key = b["idempotency_key"].as_str().unwrap().to_owned();
    let (status, answer) = {
        let mut charges = f.charges.lock().unwrap();
        match charges.get(&key) {
            Some((req, _, _)) if *req != b => {
                return (StatusCode::CONFLICT, Json(serde_json::json!({"error": "different charge"})));
            }
            Some((_, s, a)) => (*s, a.clone()),
            None => {
                let (s, a) = if b["card_token"] == "tok_visa" {
                    let id = format!("ch_test_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst));
                    (StatusCode::CREATED, serde_json::json!({"id": id, "status": "succeeded"}))
                } else {
                    (StatusCode::PAYMENT_REQUIRED, serde_json::json!({"status": "declined", "reason": "card declined"}))
                };
                charges.insert(key, (b, s, a.clone()));
                (s, a)
            }
        }
    };
    if mode == LOSES_ANSWERS {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "lost"})));
    }
    (status, Json(answer))
}

#[derive(serde::Deserialize)]
struct ByKey {
    idempotency_key: String,
}

async fn lookup(
    State(f): State<FakePayment>,
    axum::extract::Query(q): axum::extract::Query<ByKey>,
) -> (StatusCode, Json<serde_json::Value>) {
    match f.charges.lock().unwrap().get(&q.idempotency_key) {
        Some((_, _, answer)) => (StatusCode::OK, Json(answer.clone())),
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "no such charge"}))),
    }
}

async fn fake_payment(f: FakePayment) -> SocketAddr {
    let app = axum::Router::new().route("/charges", axum::routing::post(charge).get(lookup)).with_state(f);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

struct T {
    c: ShopClient<Channel>,
    payment: FakePayment,
    service: ShopService,
}

async fn start() -> T {
    let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL (run the tests with `make test`)");
    let until = Instant::now() + Duration::from_secs(180);
    let pool = loop {
        match MySqlPoolOptions::new().max_connections(8).connect(&url).await {
            Ok(p) => break p,
            Err(e) if Instant::now() > until => panic!("database: {e}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    };
    let payment = FakePayment::default();
    let addr = fake_payment(payment.clone()).await;
    let service = ShopService::new(
        pool,
        PaymentClient::new(format!("http://{addr}"), Duration::from_secs(5)),
        Sessions::new(SECRET, 3600).unwrap(),
        Passwords::new(4, Duration::from_secs(30)),
    );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let served = service.clone();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ShopServer::new(served))
            .serve_with_incoming(TcpListenerStream::new(l))
            .await
            .unwrap()
    });
    T { c: ShopClient::connect(format!("http://{addr}")).await.unwrap(), payment, service }
}

fn email(name: &str) -> String {
    format!("{name}-{}@example.com", std::process::id())
}

/// テストごとに別の顧客を作り、そのセッションのトークンを返す。
async fn sign_up(c: &mut ShopClient<Channel>, name: &str) -> String {
    c.sign_up(SignUpRequest {
        email: email(name),
        password: "password".to_owned(),
        full_name: "試験 太郎".to_owned(),
        address: "東京都".to_owned(),
    })
    .await
    .unwrap()
    .into_inner()
    .token
}

fn key(n: u32) -> String {
    format!("00000000-0000-4000-8000-{:04x}{n:08x}", std::process::id() & 0xffff)
}

fn order(token: &str, request_key: String, lines: &[(u64, u32)], card: &str) -> PlaceOrderRequest {
    PlaceOrderRequest {
        token: token.to_owned(),
        request_key,
        lines: lines.iter().map(|&(product_id, quantity)| OrderLine { product_id, quantity }).collect(),
        card_token: card.to_owned(),
    }
}

async fn stock(c: &mut ShopClient<Channel>, id: u64) -> i32 {
    let list = c.list_products(ListProductsRequest {}).await.unwrap().into_inner().products;
    list.iter().find(|p| p.id == id).unwrap().stock
}

async fn orders(c: &mut ShopClient<Channel>, token: &str) -> Vec<Order> {
    c.list_orders(ListOrdersRequest { token: token.to_owned() }).await.unwrap().into_inner().orders
}

#[tokio::test]
async fn customers_sign_up_once_per_email_and_log_in_case_insensitively() {
    let T { mut c, .. } = start().await;
    let token = sign_up(&mut c, "once").await;
    let me = c.me(MeRequest { token }).await.unwrap().into_inner();
    assert_eq!(me.email, email("once"));
    let again = c
        .sign_up(SignUpRequest {
            email: email("ONCE").to_uppercase(),
            password: "password".to_owned(),
            full_name: "x".to_owned(),
            address: "x".to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!((again.code(), again.message()), (Code::AlreadyExists, "email taken"));
    let s = c
        .log_in(LogInRequest { email: format!("  {}", email("once").to_uppercase()), password: "password".to_owned() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(s.customer.unwrap().email, email("once"));
    assert_eq!(s.max_age, 3600);
    for (e, p) in [(email("once"), "wrong password"), ("nobody@example.com".to_owned(), "password")] {
        let err = c.log_in(LogInRequest { email: e, password: p.to_owned() }).await.unwrap_err();
        assert_eq!((err.code(), err.message()), (Code::Unauthenticated, "wrong credentials"));
    }
}

#[tokio::test]
async fn logging_out_ends_every_session_of_the_customer() {
    let T { mut c, .. } = start().await;
    let first = sign_up(&mut c, "logout").await;
    let second = c
        .log_in(LogInRequest { email: email("logout"), password: "password".to_owned() })
        .await
        .unwrap()
        .into_inner()
        .token;
    c.log_out(LogOutRequest { token: first.clone() }).await.unwrap();
    for t in [first, second] {
        assert_eq!(c.me(MeRequest { token: t }).await.unwrap_err().code(), Code::Unauthenticated);
    }
    // ログインし直せる
    let third = c
        .log_in(LogInRequest { email: email("logout"), password: "password".to_owned() })
        .await
        .unwrap()
        .into_inner()
        .token;
    assert!(c.me(MeRequest { token: third }).await.is_ok());
    // 無効なトークンでのログアウトも成功する
    assert!(c.log_out(LogOutRequest { token: "garbage".to_owned() }).await.is_ok());
}

#[tokio::test]
async fn tokens_cannot_be_forged_for_another_customer() {
    let T { mut c, .. } = start().await;
    let token = sign_up(&mut c, "forger").await;
    // 顧客 ID を書き換えると署名が合わない
    let (id, rest) = token.split_once('.').unwrap();
    let other = format!("{}.{rest}", id.parse::<u64>().unwrap() - 1);
    assert_eq!(c.me(MeRequest { token: other.clone() }).await.unwrap_err().code(), Code::Unauthenticated);
    assert_eq!(c.list_orders(ListOrdersRequest { token: other }).await.unwrap_err().code(), Code::Unauthenticated);
    // 別の鍵で署名したものも通らない
    let forged = Sessions::new(b"some other secret that is long enough", 3600)
        .unwrap()
        .issue(shop_proxy::auth::Claim { customer_id: 1, epoch: 0 }, 1);
    assert_eq!(c.me(MeRequest { token: forged }).await.unwrap_err().code(), Code::Unauthenticated);
}

#[tokio::test]
async fn products_are_listed() {
    let T { mut c, .. } = start().await;
    let list = c.list_products(ListProductsRequest {}).await.unwrap().into_inner().products;
    assert!(list.iter().any(|p| p.code == "TSHIRT-001" && p.price == 2500));
}

#[tokio::test]
async fn an_order_takes_prices_from_the_database_and_is_idempotent() {
    let T { mut c, payment, .. } = start().await;
    let token = sign_up(&mut c, "idem").await;
    let before = stock(&mut c, 10).await;
    let req = order(&token, key(1), &[(10, 3)], "tok_visa");
    let first = c.place_order(req.clone()).await.unwrap().into_inner();
    let second = c.place_order(req).await.unwrap().into_inner();
    assert_eq!(first, second);
    assert_eq!((first.total, first.status.as_str()), (3000, "paid"));
    assert_eq!(first.items[0].unit_price, 1000);
    assert_eq!(stock(&mut c, 10).await, before - 3);
    assert_eq!(payment.calls(), 1, "the second request does not charge again");
    assert_eq!(orders(&mut c, &token).await, vec![first]);
}

#[tokio::test]
async fn a_request_key_reused_for_a_different_order_is_refused() {
    let T { mut c, .. } = start().await;
    let token = sign_up(&mut c, "reuse").await;
    c.place_order(order(&token, key(2), &[(1, 1)], "tok_visa")).await.unwrap();
    let e = c.place_order(order(&token, key(2), &[(1, 2)], "tok_visa")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::AlreadyExists, "request_key reused"));
}

#[tokio::test]
async fn the_same_product_on_two_lines_is_reserved_and_limited_together() {
    let T { mut c, .. } = start().await;
    let token = sign_up(&mut c, "lines").await;
    let before = stock(&mut c, 12).await;
    let o = c.place_order(order(&token, key(3), &[(12, 2), (12, 5)], "tok_visa")).await.unwrap().into_inner();
    assert_eq!((o.items.len(), o.total), (2, 7 * 250));
    assert_eq!(stock(&mut c, 12).await, before - 7);
    // 1 行ずつは 99 以下でも、同じ商品の合計が 99 を超えれば断る
    let e = c.place_order(order(&token, key(4), &[(12, 60), (12, 60)], "tok_visa")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::InvalidArgument, "invalid quantity"));
}

#[tokio::test]
async fn out_of_stock_writes_nothing_and_a_declined_payment_returns_the_stock() {
    let T { mut c, .. } = start().await;
    let token = sign_up(&mut c, "fail").await;
    let e = c.place_order(order(&token, key(5), &[(4, 1)], "tok_visa")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::FailedPrecondition, "out of stock: ステッカー"));
    let before = stock(&mut c, 15).await;
    let e = c.place_order(order(&token, key(6), &[(15, 2), (15, 1)], "tok_declined")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::FailedPrecondition, "payment declined: card declined"));
    assert_eq!(stock(&mut c, 15).await, before);
    assert!(orders(&mut c, &token).await.is_empty(), "declined orders are not shown");
    // 同じ依頼は拒否のまま（在庫を 2 度戻さない）。新しい依頼なら通る
    let e = c.place_order(order(&token, key(6), &[(15, 2), (15, 1)], "tok_visa")).await.unwrap_err();
    assert_eq!(e.code(), Code::FailedPrecondition);
    assert_eq!(stock(&mut c, 15).await, before);
    c.place_order(order(&token, key(7), &[(15, 3)], "tok_visa")).await.unwrap();
    assert_eq!(stock(&mut c, 15).await, before - 3);
}

#[tokio::test]
async fn an_unreachable_payment_leaves_the_order_pending_until_the_request_is_retried() {
    let T { mut c, payment, .. } = start().await;
    let token = sign_up(&mut c, "pending").await;
    let before = stock(&mut c, 13).await;
    payment.set(DOWN);
    let req = order(&token, key(8), &[(13, 2)], "tok_visa");
    let e = c.place_order(req.clone()).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::Unavailable, "payment unavailable"));
    let mine = orders(&mut c, &token).await;
    assert_eq!((mine.len(), mine[0].status.as_str()), (1, "pending"));
    assert_eq!(stock(&mut c, 13).await, before - 2, "the stock stays reserved");
    payment.set(UP);
    let o = c.place_order(req).await.unwrap().into_inner();
    assert_eq!((o.id, o.status.as_str()), (mine[0].id, "paid"));
    assert_eq!(stock(&mut c, 13).await, before - 2, "the stock is taken once");
}

#[tokio::test]
async fn a_lost_answer_is_settled_without_charging_twice() {
    let T { mut c, payment, .. } = start().await;
    let token = sign_up(&mut c, "lost").await;
    payment.set(LOSES_ANSWERS);
    let req = order(&token, key(9), &[(1, 1)], "tok_visa");
    assert_eq!(c.place_order(req.clone()).await.unwrap_err().code(), Code::Unavailable);
    payment.set(UP);
    // 決済サービスは前の請求を覚えているので、別のカードではやり直せない（二重に請求しない）
    let e = c.place_order(order(&token, key(9), &[(1, 1)], "tok_declined")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::FailedPrecondition, "payment conflict"));
    let o = c.place_order(req).await.unwrap().into_inner();
    assert_eq!(o.status, "paid");
    assert_eq!(payment.charges.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_same_request_sent_twice_at_once_makes_one_order() {
    let T { mut c, payment, .. } = start().await;
    let token = sign_up(&mut c, "twice").await;
    let before = stock(&mut c, 14).await;
    let req = order(&token, key(10), &[(14, 4)], "tok_visa");
    let (mut a, mut b) = (c.clone(), c.clone());
    let (ra, rb) = tokio::join!(a.place_order(req.clone()), b.place_order(req));
    let (oa, ob) = (ra.unwrap().into_inner(), rb.unwrap().into_inner());
    assert_eq!(oa.id, ob.id);
    assert_eq!(stock(&mut c, 14).await, before - 4);
    assert_eq!(payment.charges.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn bad_requests_are_rejected() {
    let T { mut c, .. } = start().await;
    let token = sign_up(&mut c, "bad").await;
    for (req, code) in [
        (order(&token, "not-a-uuid".to_owned(), &[(1, 1)], "tok_visa"), Code::InvalidArgument),
        (order(&token, key(11).to_uppercase(), &[(1, 1)], "tok_visa"), Code::InvalidArgument),
        (order(&token, key(12), &[(1, 0)], "tok_visa"), Code::InvalidArgument),
        (order(&token, key(13), &[(1, 100)], "tok_visa"), Code::InvalidArgument),
        (order(&token, key(14), &[], "tok_visa"), Code::InvalidArgument),
        (order(&token, key(15), &[(999, 1)], "tok_visa"), Code::InvalidArgument),
        (order("garbage", key(16), &[(1, 1)], "tok_visa"), Code::Unauthenticated),
    ] {
        assert_eq!(c.place_order(req).await.unwrap_err().code(), code);
    }
    let weak = SignUpRequest {
        email: email("weak"),
        password: "short".to_owned(),
        full_name: "x".to_owned(),
        address: "y".to_owned(),
    };
    assert_eq!(c.sign_up(weak).await.unwrap_err().message(), "invalid password");
}

#[tokio::test]
async fn customers_see_only_their_own_orders() {
    let T { mut c, .. } = start().await;
    let alice = sign_up(&mut c, "alice").await;
    let eve = sign_up(&mut c, "eve").await;
    let o = c.place_order(order(&alice, key(17), &[(1, 1)], "tok_visa")).await.unwrap().into_inner();
    let e = c.get_order(GetOrderRequest { token: eve.clone(), order_id: o.id }).await.unwrap_err();
    assert_eq!(e.code(), Code::NotFound);
    assert!(orders(&mut c, &eve).await.is_empty());
    assert_eq!(c.get_order(GetOrderRequest { token: alice, order_id: o.id }).await.unwrap().into_inner(), o);
}

#[tokio::test]
async fn an_old_token_cannot_end_newer_sessions() {
    let T { mut c, .. } = start().await;
    let old = sign_up(&mut c, "stale-token").await;
    c.log_out(LogOutRequest { token: old.clone() }).await.unwrap();
    let new = c
        .log_in(LogInRequest { email: email("stale-token"), password: "password".to_owned() })
        .await
        .unwrap()
        .into_inner()
        .token;
    // ログアウト済みのトークンでのログアウトは何もしない
    c.log_out(LogOutRequest { token: old }).await.unwrap();
    assert!(c.me(MeRequest { token: new }).await.is_ok());
}

#[tokio::test]
async fn repeated_wrong_passwords_lock_the_email_for_a_while() {
    let T { mut c, .. } = start().await;
    sign_up(&mut c, "throttled").await;
    let attempt = |password: &str| LogInRequest { email: email("throttled"), password: password.to_owned() };
    for _ in 0..10 {
        assert_eq!(c.log_in(attempt("wrong password")).await.unwrap_err().code(), Code::Unauthenticated);
    }
    let e = c.log_in(attempt("password")).await.unwrap_err();
    assert_eq!((e.code(), e.message()), (Code::ResourceExhausted, "too many attempts"));
}

#[tokio::test]
async fn stale_pending_orders_are_reconciled_with_the_payment_service() {
    let T { mut c, payment, service } = start().await;
    // 1001 の請求は決済サービスに届いていた（答えが失われた）。1002 は届いていない
    payment.charges.lock().unwrap().insert(
        "11111111-1111-4111-8111-111111111001".to_owned(),
        (serde_json::Value::Null, StatusCode::CREATED, serde_json::json!({"id": "ch_stale", "status": "succeeded"})),
    );
    let before = stock(&mut c, 16).await;
    // 1 時間より古いものだけ（ほかのテストの新しい pending の注文には触らない）
    assert_eq!(service.reconcile(3600).await.unwrap(), 2);
    assert_eq!(stock(&mut c, 16).await, before + 2, "the order without a charge returns its stock");
    assert_eq!(service.reconcile(3600).await.unwrap(), 0, "nothing is left");
    assert_eq!(payment.calls(), 0, "reconciling never charges");
}
