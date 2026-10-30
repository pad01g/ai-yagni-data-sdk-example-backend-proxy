//! backend-rust のための、固定クエリだけを持つ SQL のマイクロサービス（gRPC）。
//!
//! djb 流の分割: backend-rust は DB にも決済サービスにも直接つながらず、`shop.v1.Shop` の操作だけを呼ぶ。
//! proxy は SQL の文字列を受け取らず、`queries/*.sql` の固定クエリに値を束縛して投げるだけ。
//!
//! 顧客の認証も proxy がする（[`auth`]）。顧客を名指しする操作は、顧客 ID ではなく proxy が署名した
//! セッションのトークンを受け取る。backend-rust が乗っ取られても、ログインしている顧客のデータにしか触れず、
//! パスワードのハッシュも読めない。
//!
//! 注文の金額と在庫は proxy が DB の値から決める（Verus で証明した `proxy_verified` を使う）。
//! 注文は 2 段で書く（[`ShopService::place`]）。決済サービスを待つあいだ、行の固定もトランザクションも持たない。
pub mod auth;
pub mod payment;
pub mod queries;
pub mod throttle;

use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::password_hash::rand_core::{OsRng, RngCore};

use shop_proxy_api::shop_server::Shop;
use shop_proxy_api::{
    Customer, GetOrderRequest, ListOrdersRequest, ListProductsRequest, LogInRequest, LogOutRequest, LogOutResponse,
    MeRequest, Order, OrderItem, OrderList, PlaceOrderRequest, Product, ProductList, Session, SignUpRequest,
};
use sqlx::Row;
use sqlx::mysql::{MySqlDatabaseError, MySqlPool, MySqlRow};
use tonic::{Request, Response, Status};

use crate::auth::{Busy, Claim, Passwords, Sessions};
use crate::payment::{Charge, PaymentClient, PaymentError, public_reason};
use crate::throttle::Throttle;

/// 1 つの注文の明細の行数の上限。
pub const MAX_LINES: usize = 20;
/// 注文の通貨（今は円だけ）。
pub const CURRENCY: &str = "JPY";

struct Inner {
    pool: MySqlPool,
    payment: PaymentClient,
    sessions: Sessions,
    passwords: Passwords,
    /// メールごとのログインの失敗（15 分に 10 回まで）
    logins: Throttle,
    /// 顧客ごとの注文の依頼（10 分に 30 回まで）
    orders: Throttle,
}

#[derive(Clone)]
pub struct ShopService(Arc<Inner>);

impl ShopService {
    pub fn new(pool: MySqlPool, payment: PaymentClient, sessions: Sessions, passwords: Passwords) -> Self {
        Self(Arc::new(Inner {
            pool,
            payment,
            sessions,
            passwords,
            logins: Throttle::new(10, Duration::from_secs(15 * 60)),
            orders: Throttle::new(30, Duration::from_secs(10 * 60)),
        }))
    }

    fn pool(&self) -> &MySqlPool {
        &self.0.pool
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------------------- 失敗

/// 詳しいことはログにだけ書き、呼ぶ側には "internal" とだけ返す。
fn internal(context: &str, e: impl Display) -> Status {
    eprintln!("proxy: {context}: {e}");
    Status::internal("internal")
}

fn invalid(field: &str) -> Status {
    Status::invalid_argument(format!("invalid {field}"))
}

fn unauthenticated() -> Status {
    Status::unauthenticated("unauthenticated")
}

fn busy(_: Busy) -> Status {
    Status::unavailable("busy")
}

fn too_many() -> Status {
    Status::resource_exhausted("too many attempts")
}

/// やり直せば通りうる DB の失敗（行の固定の待ちの時間切れ、デッドロック）。
fn transient(e: &sqlx::Error) -> bool {
    matches!(mysql_error(e), Some(1205 | 1213))
}

/// 決済サービスへの冪等キー（乱数の UUID）。注文の ID と違い、DB を作り直しても前のものと重ならない。
fn new_payment_key() -> String {
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// MySQL のエラー番号。
fn mysql_error(e: &sqlx::Error) -> Option<u16> {
    match e {
        sqlx::Error::Database(d) => d.try_downcast_ref::<MySqlDatabaseError>().map(|m| m.number()),
        _ => None,
    }
}

/// 一意キー key の重複か（1062 の文言にキーの名前が入る）。
fn duplicate_on(e: &sqlx::Error, key: &str) -> bool {
    mysql_error(e) == Some(1062) && e.to_string().contains(key)
}

/// DB の失敗。混んでいるだけ（行の固定の待ちの時間切れ、デッドロック、接続の空き待ち）ならやり直してよい。
fn db(e: sqlx::Error) -> Status {
    match (mysql_error(&e), &e) {
        (Some(1205 | 1213), _) | (_, sqlx::Error::PoolTimedOut) => Status::unavailable("busy"),
        _ => internal("database", e),
    }
}

fn check_text(field: &str, v: &str, max: usize) -> Result<(), Status> {
    let n = v.chars().count();
    if n == 0 || n > max || v.chars().any(char::is_control) {
        return Err(invalid(field));
    }
    Ok(())
}

/// 依頼の冪等キー: 小文字の UUID の形（8-4-4-4-12 の 16 進）。大文字と小文字で別の依頼にならないように、
/// 小文字だけを受け付ける。
fn valid_request_key(k: &str) -> bool {
    k.len() == 36
        && k.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

// ---------------------------------------------------------------------------------------- 行

/// 文字列の列。照合順序が *_bin の列は、sqlx に VARBINARY として報告されるので、バイト列からも読む。
fn text(r: &MySqlRow, column: &str) -> Result<String, sqlx::Error> {
    match r.try_get::<String, _>(column) {
        Ok(s) => Ok(s),
        Err(_) => {
            let bytes: Vec<u8> = r.try_get(column)?;
            String::from_utf8(bytes).map_err(|e| sqlx::Error::Decode(Box::new(e)))
        }
    }
}

fn customer(r: &MySqlRow) -> Result<Customer, sqlx::Error> {
    Ok(Customer { email: text(r, "email")?, full_name: text(r, "full_name")?, address: text(r, "address")? })
}

fn product(r: &MySqlRow) -> Result<Product, sqlx::Error> {
    Ok(Product {
        id: r.try_get("id")?,
        code: text(r, "code")?,
        name: text(r, "name")?,
        description: text(r, "description")?,
        price: r.try_get("price")?,
        stock: r.try_get("stock")?,
    })
}

fn order(r: &MySqlRow) -> Result<Order, sqlx::Error> {
    Ok(Order {
        id: r.try_get("id")?,
        total: r.try_get("total")?,
        status: text(r, "status")?,
        created_at: text(r, "created_at")?,
        items: Vec::new(),
    })
}

/// 明細と、その注文の ID。
fn item(r: &MySqlRow) -> Result<(u64, OrderItem), sqlx::Error> {
    let i = OrderItem {
        line_no: u32::from(r.try_get::<u16, _>("line_no")?),
        product_id: r.try_get("product_id")?,
        product_name: text(r, "product_name")?,
        quantity: r.try_get("quantity")?,
        unit_price: r.try_get("unit_price")?,
    };
    Ok((r.try_get("order_id")?, i))
}

/// 書いた注文（決着の前かもしれない）。
#[derive(Debug, Clone)]
struct Placed {
    id: u64,
    total: u64,
    status: String,
    /// 決済サービスへの冪等キー（pending の注文は必ず持つ）
    payment_key: Option<String>,
}

fn placed(row: &MySqlRow) -> Result<Placed, sqlx::Error> {
    let payment_key = match row.try_get::<Option<String>, _>("payment_key") {
        Ok(k) => k,
        Err(_) => row.try_get::<Option<Vec<u8>>, _>("payment_key")?.map(|b| String::from_utf8_lossy(&b).into_owned()),
    };
    Ok(Placed { id: row.try_get("id")?, total: row.try_get("total")?, status: text(row, "status")?, payment_key })
}

// ---------------------------------------------------------------------------------------- 操作の中身

impl ShopService {
    /// トークンを検証し、今の世代のセッションなら顧客 ID。
    async fn authenticate(&self, token: &str) -> Result<u64, Status> {
        let claim = self.0.sessions.verify(token, now()).ok_or_else(unauthenticated)?;
        let row = sqlx::query(queries::CUSTOMER_SESSION)
            .bind(claim.customer_id)
            .fetch_optional(self.pool())
            .await
            .map_err(db)?
            .ok_or_else(unauthenticated)?;
        let epoch: u32 = row.try_get("session_epoch").map_err(db)?;
        if epoch != claim.epoch {
            return Err(unauthenticated());
        }
        Ok(claim.customer_id)
    }

    async fn customer(&self, id: u64) -> Result<Customer, Status> {
        let row = sqlx::query(queries::CUSTOMER_BY_ID)
            .bind(id)
            .fetch_optional(self.pool())
            .await
            .map_err(db)?
            .ok_or_else(unauthenticated)?;
        customer(&row).map_err(db)
    }

    fn session(&self, claim: Claim, customer: Customer) -> Session {
        Session { token: self.0.sessions.issue(claim, now()), max_age: self.0.sessions.ttl, customer: Some(customer) }
    }

    async fn load_order(&self, customer_id: u64, id: u64) -> Result<Order, Status> {
        let row = sqlx::query(queries::ORDER_BY_ID)
            .bind(id)
            .bind(customer_id)
            .fetch_optional(self.pool())
            .await
            .map_err(db)?
            .ok_or_else(|| Status::not_found("no such order"))?;
        let mut o = order(&row).map_err(db)?;
        let rows = sqlx::query(queries::ORDER_ITEMS_BY_ORDER).bind(id).fetch_all(self.pool()).await.map_err(db)?;
        o.items = rows.iter().map(|r| item(r).map(|(_, i)| i)).collect::<Result<_, _>>().map_err(db)?;
        Ok(o)
    }

    /// 同じ依頼の注文が既にあれば返す。同じキーで明細が違えば ALREADY_EXISTS（前の注文を黙って返さない）。
    async fn existing(&self, customer_id: u64, req: &PlaceOrderRequest) -> Result<Option<Placed>, Status> {
        let Some(row) = sqlx::query(queries::ORDER_BY_REQUEST)
            .bind(customer_id)
            .bind(&req.request_key)
            .fetch_optional(self.pool())
            .await
            .map_err(db)?
        else {
            return Ok(None);
        };
        let placed = placed(&row).map_err(db)?;
        let rows =
            sqlx::query(queries::ORDER_ITEMS_BY_ORDER).bind(placed.id).fetch_all(self.pool()).await.map_err(db)?;
        let mut before = Vec::with_capacity(rows.len());
        for r in &rows {
            let (_, i) = item(r).map_err(db)?;
            before.push((i.product_id, i.quantity));
        }
        let now: Vec<(u64, u32)> = req.lines.iter().map(|l| (l.product_id, l.quantity)).collect();
        if before != now {
            return Err(Status::already_exists("request_key reused"));
        }
        Ok(Some(placed))
    }

    /// 1 つ目のトランザクション: 商品の行を固定し、在庫を引き当て、DB の価格で合計を決め、注文を pending で書く。
    async fn reserve(&self, customer_id: u64, req: &PlaceOrderRequest) -> Result<Placed, Status> {
        // 同じ商品の明細は足し合わせて引き当てる。行を固定する順序をそろえる（デッドロックを避ける）
        let mut wanted: BTreeMap<u64, u32> = BTreeMap::new();
        for l in &req.lines {
            let w = wanted.entry(l.product_id).or_insert(0);
            *w = w.checked_add(l.quantity).ok_or_else(|| invalid("quantity"))?;
        }
        if wanted.values().any(|&w| !proxy_verified::valid_quantity(w)) {
            return Err(invalid("quantity"));
        }
        let mut tx = self.pool().begin().await.map_err(db)?;
        let mut prices: BTreeMap<u64, u32> = BTreeMap::new();
        let mut stocks: Vec<(u64, u32, u32)> = Vec::new();
        for (&id, &want) in &wanted {
            let row = sqlx::query(queries::PRODUCT_LOCK)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or_else(|| invalid("product_id"))?;
            let stock: i32 = row.try_get("stock").map_err(db)?;
            let stock = u32::try_from(stock).map_err(|_| internal("negative stock", id))?;
            let Some(left) = proxy_verified::reserve(stock, want) else {
                let name = text(&row, "name").map_err(db)?;
                return Err(Status::failed_precondition(format!("out of stock: {name}")));
            };
            stocks.push((id, stock, left));
            prices.insert(id, row.try_get("price").map_err(db)?);
        }
        let line_prices: Vec<u32> = req.lines.iter().map(|l| prices[&l.product_id]).collect();
        let quantities: Vec<u32> = req.lines.iter().map(|l| l.quantity).collect();
        let total = proxy_verified::order_total(&line_prices, &quantities).ok_or_else(|| invalid("quantity"))?;

        let payment_key = new_payment_key();
        let inserted = sqlx::query(queries::ORDER_INSERT)
            .bind(customer_id)
            .bind(&req.request_key)
            .bind(total)
            .bind(&payment_key)
            .execute(&mut *tx)
            .await;
        let order_id = match inserted {
            Ok(r) => r.last_insert_id(),
            Err(e) if duplicate_on(&e, "orders_request") => {
                // 同じ依頼が同時に来て、先に書いた方がある
                drop(tx);
                return self.existing(customer_id, req).await?.ok_or_else(|| Status::unavailable("busy"));
            }
            Err(e) if mysql_error(&e) == Some(1452) => return Err(unauthenticated()),
            Err(e) => return Err(db(e)),
        };
        for (i, (l, price)) in req.lines.iter().zip(&line_prices).enumerate() {
            let line_no = u16::try_from(i + 1).map_err(|_| invalid("lines"))?;
            sqlx::query(queries::ORDER_ITEM_INSERT)
                .bind(order_id)
                .bind(line_no)
                .bind(l.product_id)
                .bind(l.quantity)
                .bind(price)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        for (id, stock, left) in stocks {
            let r = sqlx::query(queries::PRODUCT_SET_STOCK)
                .bind(left)
                .bind(id)
                .bind(stock)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            if r.rows_affected() != 1 {
                return Err(internal("stock changed under the row lock", id));
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(Placed { id: order_id, total, status: "pending".to_owned(), payment_key: Some(payment_key) })
    }

    /// 決済の拒否（または決済がなかった）: 注文を declined にし、引き当てた在庫を戻す（1 つのトランザクション）。
    /// 商品の行は引き当てと同じ ID の順に固定する。デッドロックなどはその場で 3 度までやり直す。
    async fn decline(&self, order_id: u64) -> Result<(), Status> {
        let once = || async {
            let mut tx = self.pool().begin().await?;
            sqlx::query(queries::PRODUCT_LOCK_ORDER).bind(order_id).fetch_all(&mut *tx).await?;
            let r = sqlx::query(queries::ORDER_MARK_DECLINED).bind(order_id).execute(&mut *tx).await?;
            if r.rows_affected() == 1 {
                sqlx::query(queries::PRODUCT_RESTOCK_ORDER).bind(order_id).execute(&mut *tx).await?;
            }
            tx.commit().await
        };
        let mut attempt = 0;
        loop {
            match once().await {
                Err(e) if transient(&e) && attempt < 2 => attempt += 1,
                r => return r.map_err(db),
            }
        }
    }

    /// 決済が通った注文を paid にする。pending でなければ何もしない（false）。やり直しは decline と同じ。
    async fn mark_paid(&self, order_id: u64, charge: &str) -> Result<bool, Status> {
        let mut attempt = 0;
        loop {
            match sqlx::query(queries::ORDER_MARK_PAID).bind(charge).bind(order_id).execute(self.pool()).await {
                Ok(done) => return Ok(done.rows_affected() == 1),
                Err(e) if transient(&e) && attempt < 2 => attempt += 1,
                Err(e) if duplicate_on(&e, "orders_payment") => {
                    return Err(internal(
                        "a charge is already used by another order",
                        format!("{charge} for order {order_id}"),
                    ));
                }
                Err(e) => return Err(db(e)),
            }
        }
    }

    /// pending の注文を、決済サービスに請求して決着させる。トランザクションの外で請求する。
    /// 冪等キーは注文ごとの payment_key なので、やり直しても二重に請求しない。
    async fn settle(&self, customer_id: u64, p: &Placed, card_token: &str) -> Result<u64, Status> {
        let key = p.payment_key.as_deref().ok_or_else(|| internal("a pending order without a payment key", p.id))?;
        match self.0.payment.charge(key, p.total, CURRENCY, card_token).await {
            Ok(Charge::Succeeded { id: charge }) => {
                if self.mark_paid(p.id, &charge).await? {
                    Ok(p.id)
                } else {
                    // ほかの依頼か、突き合わせが先に決着させた
                    self.load_order(customer_id, p.id).await.map(|o| o.id)
                }
            }
            Ok(Charge::Declined { reason }) => {
                self.decline(p.id).await?;
                Err(Status::failed_precondition(format!("payment declined: {}", public_reason(&reason))))
            }
            Err(PaymentError::Conflict) => Err(Status::failed_precondition("payment conflict")),
            Err(PaymentError::Unavailable(e)) => {
                eprintln!("proxy: payment unavailable for order {}: {e}", p.id);
                Err(Status::unavailable("payment unavailable"))
            }
            Err(PaymentError::Invalid(e)) => Err(internal("payment refused the request", e)),
        }
    }

    /// 決着しないまま older_than 秒より古くなった pending の注文を、決済サービスの記録と突き合わせて片付ける。
    /// - 決済が通っていれば paid にする（請求は済んでいる）。
    /// - 拒否されていたか、決済の記録がなければ declined にして在庫を戻す（利用者は結果を受け取っていないので、
    ///   新たに請求はしない）。
    ///
    /// 片付けた注文の数を返す。決済サービスに届かなければ、そこでやめる（次の回にやり直す）。
    pub async fn reconcile(&self, older_than: u64) -> Result<usize, Status> {
        let rows =
            sqlx::query(queries::ORDERS_PENDING_STALE).bind(older_than).fetch_all(self.pool()).await.map_err(db)?;
        let mut settled = 0;
        for row in &rows {
            let p = placed(row).map_err(db)?;
            let found = match p.payment_key.as_deref() {
                Some(key) => self.0.payment.find(key).await,
                None => Ok(None),
            };
            match found {
                Ok(Some(Charge::Succeeded { id: charge })) => {
                    self.mark_paid(p.id, &charge).await?;
                }
                Ok(Some(Charge::Declined { .. }) | None) => self.decline(p.id).await?,
                Err(e) => {
                    eprintln!("proxy: cannot reconcile order {}: {e:?}", p.id);
                    break;
                }
            }
            settled += 1;
        }
        Ok(settled)
    }

    /// 注文する。
    /// 1. 同じ依頼の注文があればそれを使う。なければ在庫を引き当てて pending で書く（[`Self::reserve`]）。
    /// 2. pending なら決済サービスに請求し、paid か declined に決着させる（[`Self::settle`]）。
    ///
    /// 決済サービスに届かなければ注文は pending のまま残り、引き当てた在庫も残る。同じ依頼をやり直すと決着する。
    async fn place(&self, customer_id: u64, req: &PlaceOrderRequest) -> Result<u64, Status> {
        let placed = match self.existing(customer_id, req).await? {
            Some(p) => p,
            None => self.reserve(customer_id, req).await?,
        };
        match placed.status.as_str() {
            "pending" => self.settle(customer_id, &placed, &req.card_token).await,
            "declined" => Err(Status::failed_precondition("payment declined: declined")),
            _ => Ok(placed.id),
        }
    }
}

// ---------------------------------------------------------------------------------------- gRPC

#[tonic::async_trait]
impl Shop for ShopService {
    async fn sign_up(&self, request: Request<SignUpRequest>) -> Result<Response<Session>, Status> {
        let r = request.into_inner();
        let email = auth::normalize_email(&r.email).ok_or_else(|| invalid("email"))?;
        let n = r.password.chars().count();
        if !(auth::MIN_PASSWORD..=auth::MAX_PASSWORD).contains(&n) {
            return Err(invalid("password"));
        }
        let (full_name, address) = (r.full_name.trim(), r.address.trim());
        check_text("full_name", full_name, 100)?;
        check_text("address", address, 500)?;
        let password_hash = self.0.passwords.hash(r.password).await.map_err(busy)?;
        let res = sqlx::query(queries::CUSTOMER_INSERT)
            .bind(&email)
            .bind(&password_hash)
            .bind(full_name)
            .bind(address)
            .execute(self.pool())
            .await;
        let id = match res {
            Ok(done) => done.last_insert_id(),
            Err(e) if duplicate_on(&e, "customers_email") => return Err(Status::already_exists("email taken")),
            Err(e) if mysql_error(&e) == Some(3819) => return Err(invalid("email")),
            Err(e) => return Err(db(e)),
        };
        let c = Customer { email, full_name: full_name.to_owned(), address: address.to_owned() };
        Ok(Response::new(self.session(Claim { customer_id: id, epoch: 0 }, c)))
    }

    async fn log_in(&self, request: Request<LogInRequest>) -> Result<Response<Session>, Status> {
        let r = request.into_inner();
        let wrong = || Status::unauthenticated("wrong credentials");
        // 形の誤りと長すぎるパスワードは、照合せずに断る（重い計算をさせない）
        let email = auth::normalize_email(&r.email).ok_or_else(wrong)?;
        if r.password.chars().count() > auth::MAX_PASSWORD {
            return Err(wrong());
        }
        // 同じメールへの失敗が続いたら、しばらく照合しない（IP を替えながらの総当たりを遅くする）
        if self.0.logins.exhausted(&email) {
            return Err(too_many());
        }
        let row =
            sqlx::query(queries::CREDENTIAL_BY_EMAIL).bind(&email).fetch_optional(self.pool()).await.map_err(db)?;
        let found = match &row {
            Some(row) => Some((
                row.try_get::<u64, _>("id").map_err(db)?,
                row.try_get::<u32, _>("session_epoch").map_err(db)?,
                text(row, "password_hash").map_err(db)?,
            )),
            None => None,
        };
        let hash = found.as_ref().map(|(_, _, h)| h.clone());
        if !self.0.passwords.verify(r.password, hash).await.map_err(busy)? {
            self.0.logins.count(&email);
            return Err(wrong());
        }
        self.0.logins.forget(&email);
        let (customer_id, epoch, _) = found.ok_or_else(wrong)?;
        let c = self.customer(customer_id).await?;
        Ok(Response::new(self.session(Claim { customer_id, epoch }, c)))
    }

    async fn log_out(&self, request: Request<LogOutRequest>) -> Result<Response<LogOutResponse>, Status> {
        if let Some(claim) = self.0.sessions.verify(&request.into_inner().token, now()) {
            sqlx::query(queries::CUSTOMER_END_SESSIONS)
                .bind(claim.customer_id)
                .bind(claim.epoch)
                .execute(self.pool())
                .await
                .map_err(db)?;
        }
        Ok(Response::new(LogOutResponse {}))
    }

    async fn me(&self, request: Request<MeRequest>) -> Result<Response<Customer>, Status> {
        let id = self.authenticate(&request.into_inner().token).await?;
        Ok(Response::new(self.customer(id).await?))
    }

    async fn list_products(&self, _: Request<ListProductsRequest>) -> Result<Response<ProductList>, Status> {
        let rows = sqlx::query(queries::PRODUCTS_ALL).fetch_all(self.pool()).await.map_err(db)?;
        let products = rows.iter().map(product).collect::<Result<_, _>>().map_err(db)?;
        Ok(Response::new(ProductList { products }))
    }

    async fn place_order(&self, request: Request<PlaceOrderRequest>) -> Result<Response<Order>, Status> {
        let r = request.into_inner();
        if !valid_request_key(&r.request_key) {
            return Err(invalid("request_key"));
        }
        if r.lines.is_empty() || r.lines.len() > MAX_LINES {
            return Err(invalid("lines"));
        }
        if r.lines.iter().any(|l| !proxy_verified::valid_quantity(l.quantity)) {
            return Err(invalid("quantity"));
        }
        if r.card_token.is_empty() || r.card_token.len() > 64 {
            return Err(invalid("card_token"));
        }
        let customer_id = self.authenticate(&r.token).await?;
        if self.0.orders.hit(&customer_id.to_string()) {
            return Err(too_many());
        }
        // 呼んだ側が待つのをやめても（切断・時間切れ）、決済と決着は最後まで進める
        let this = self.clone();
        let id = tokio::spawn(async move { this.place(customer_id, &r).await })
            .await
            .map_err(|e| internal("the order task failed", e))??;
        Ok(Response::new(self.load_order(customer_id, id).await?))
    }

    async fn list_orders(&self, request: Request<ListOrdersRequest>) -> Result<Response<OrderList>, Status> {
        let customer_id = self.authenticate(&request.into_inner().token).await?;
        // 注文と明細を同じスナップショットから読む
        let mut tx = self.pool().begin().await.map_err(db)?;
        let rows = sqlx::query(queries::ORDERS_BY_CUSTOMER).bind(customer_id).fetch_all(&mut *tx).await.map_err(db)?;
        let mut orders: Vec<Order> = rows.iter().map(order).collect::<Result<_, _>>().map_err(db)?;
        if let Some(oldest) = orders.iter().map(|o| o.id).min() {
            let rows = sqlx::query(queries::ORDER_ITEMS_BY_CUSTOMER)
                .bind(customer_id)
                .bind(oldest)
                .fetch_all(&mut *tx)
                .await
                .map_err(db)?;
            let mut items: BTreeMap<u64, Vec<OrderItem>> = BTreeMap::new();
            for r in &rows {
                let (order_id, i) = item(r).map_err(db)?;
                items.entry(order_id).or_default().push(i);
            }
            for o in &mut orders {
                o.items = items.remove(&o.id).unwrap_or_default();
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(Response::new(OrderList { orders }))
    }

    async fn get_order(&self, request: Request<GetOrderRequest>) -> Result<Response<Order>, Status> {
        let r = request.into_inner();
        let customer_id = self.authenticate(&r.token).await?;
        Ok(Response::new(self.load_order(customer_id, r.order_id).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_keys_are_lower_case_uuids() {
        assert!(valid_request_key("6f1c2a0e-0b6f-4c55-9d43-0d8a3c1e0001"));
        assert!(!valid_request_key("6F1C2A0E-0B6F-4C55-9D43-0D8A3C1E0001"));
        assert!(!valid_request_key("------------------------------------"));
        assert!(!valid_request_key("6f1c2a0e0b6f-4c55-9d43-0d8a3c1e0001-"));
    }

    #[test]
    fn every_query_file_is_embedded() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../queries");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        files.sort();
        let mut embedded: Vec<String> = queries::ALL.iter().map(|(f, _)| (*f).to_owned()).collect();
        embedded.sort();
        assert_eq!(files, embedded);
    }
}
