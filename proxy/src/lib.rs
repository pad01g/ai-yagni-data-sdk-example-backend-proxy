//! backend-rust のための、固定クエリだけを持つ SQL のマイクロサービス（gRPC）。
//!
//! djb 流の分割: backend-rust は DB にも決済サービスにも直接つながらず、`shop.v1.Shop` の操作だけを呼ぶ。
//! proxy は SQL の文字列を受け取らず、`queries/*.sql` の固定クエリに値を束縛して投げるだけ。
//! backend-rust が乗っ取られても、ここにない読み書き（ほかの表を読む、価格を書き換える、…）はできない。
//!
//! 注文の金額と在庫は proxy が DB の値から決める（Verus で証明した `proxy_verified` を使う）。
pub mod payment;
pub mod queries;

use std::collections::BTreeMap;

use shop_proxy_api::shop_server::Shop;
use shop_proxy_api::{
    CreateCustomerRequest, Credential, Customer, FindCredentialRequest, GetCustomerRequest, GetOrderRequest,
    GetProductRequest, ListOrdersRequest, ListProductsRequest, Order, OrderItem, OrderList, PlaceOrderRequest, Product,
    ProductList,
};
use sqlx::mysql::{MySqlDatabaseError, MySqlPool, MySqlRow};
use sqlx::{MySql, Row, Transaction};
use tonic::{Request, Response, Status};

use crate::payment::{Charge, PaymentClient};

/// 1 つの注文の明細の行数の上限。
pub const MAX_LINES: usize = 20;
/// 注文の通貨（今は円だけ）。
pub const CURRENCY: &str = "JPY";

pub struct ShopService {
    pool: MySqlPool,
    payment: PaymentClient,
}

impl ShopService {
    pub fn new(pool: MySqlPool, payment: PaymentClient) -> Self {
        Self { pool, payment }
    }
}

fn internal(e: sqlx::Error) -> Status {
    Status::internal(format!("database: {e}"))
}

/// MySQL のエラー番号（1062 = 一意キーの重複、1452 = 外部キーの相手がない）。
fn mysql_error(e: &sqlx::Error) -> Option<u16> {
    match e {
        sqlx::Error::Database(d) => d.try_downcast_ref::<MySqlDatabaseError>().map(|m| m.number()),
        _ => None,
    }
}

fn check_len(name: &str, v: &str, min: usize, max: usize) -> Result<(), Status> {
    let n = v.chars().count();
    if n < min || n > max {
        return Err(Status::invalid_argument(format!("{name} must be {min}..={max} characters")));
    }
    Ok(())
}

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
    Ok(Customer {
        id: r.try_get("id")?,
        email: text(r, "email")?,
        full_name: text(r, "full_name")?,
        address: text(r, "address")?,
    })
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
        customer_id: r.try_get("customer_id")?,
        total: r.try_get("total")?,
        status: text(r, "status")?,
        created_at: text(r, "created_at")?,
        payment_ref: text(r, "payment_ref")?,
        items: Vec::new(),
    })
}

fn item(r: &MySqlRow) -> Result<OrderItem, sqlx::Error> {
    Ok(OrderItem {
        line_no: u32::from(r.try_get::<u16, _>("line_no")?),
        product_id: r.try_get("product_id")?,
        product_name: text(r, "product_name")?,
        quantity: r.try_get("quantity")?,
        unit_price: r.try_get("unit_price")?,
    })
}

/// 依頼の冪等キー: UUID の形（36 文字の 16 進と -）。
fn valid_request_key(k: &str) -> bool {
    k.len() == 36 && k.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

impl ShopService {
    async fn load_order(&self, id: u64, customer_id: u64) -> Result<Order, Status> {
        let row = sqlx::query(queries::ORDER_BY_ID)
            .bind(id)
            .bind(customer_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("no such order"))?;
        let mut o = order(&row).map_err(internal)?;
        o.items = self.items(o.id).await?;
        Ok(o)
    }

    async fn items(&self, order_id: u64) -> Result<Vec<OrderItem>, Status> {
        let rows =
            sqlx::query(queries::ORDER_ITEMS_BY_ORDER).bind(order_id).fetch_all(&self.pool).await.map_err(internal)?;
        rows.iter().map(item).collect::<Result<_, _>>().map_err(internal)
    }

    async fn existing_order(
        tx: &mut Transaction<'_, MySql>,
        customer_id: u64,
        key: &str,
    ) -> Result<Option<u64>, Status> {
        let row = sqlx::query(queries::ORDER_BY_REQUEST)
            .bind(customer_id)
            .bind(key)
            .fetch_optional(&mut **tx)
            .await
            .map_err(internal)?;
        row.map(|r| r.try_get::<u64, _>("id")).transpose().map_err(internal)
    }

    /// 注文の本体。成功したら注文の ID を返す。
    async fn place(&self, req: &PlaceOrderRequest) -> Result<u64, Status> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        if let Some(id) = Self::existing_order(&mut tx, req.customer_id, &req.request_key).await? {
            return Ok(id);
        }
        // 行を固定する順序をそろえる（デッドロックを避ける）
        let mut wanted: BTreeMap<u64, u32> = BTreeMap::new();
        for l in &req.lines {
            let w = wanted.entry(l.product_id).or_insert(0);
            *w = w.checked_add(l.quantity).ok_or_else(|| Status::invalid_argument("quantity too large"))?;
        }
        let mut prices: BTreeMap<u64, u32> = BTreeMap::new();
        for (&id, &want) in &wanted {
            let row = sqlx::query(queries::PRODUCT_LOCK)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?
                .ok_or_else(|| Status::not_found(format!("no product {id}")))?;
            let name = text(&row, "name").map_err(internal)?;
            let stock: i32 = row.try_get("stock").map_err(internal)?;
            if proxy_verified::reserve(u32::try_from(stock).unwrap_or(0), want).is_none() {
                return Err(Status::failed_precondition(format!("out of stock: {name}")));
            }
            prices.insert(id, row.try_get("price").map_err(internal)?);
        }
        let line_prices: Vec<u32> = req.lines.iter().map(|l| prices[&l.product_id]).collect();
        let quantities: Vec<u32> = req.lines.iter().map(|l| l.quantity).collect();
        let total = proxy_verified::order_total(&line_prices, &quantities)
            .ok_or_else(|| Status::invalid_argument("total too large"))?;

        let key = format!("order-{}-{}", req.customer_id, req.request_key);
        let payment_ref = match self.payment.charge(&key, total, CURRENCY, &req.card_token).await {
            Ok(Charge::Succeeded { id }) => id,
            Ok(Charge::Declined { reason }) => {
                return Err(Status::failed_precondition(format!("payment declined: {reason}")));
            }
            Err(e) => return Err(Status::unavailable(e)),
        };

        let inserted = sqlx::query(queries::ORDER_INSERT)
            .bind(req.customer_id)
            .bind(&req.request_key)
            .bind(total)
            .bind(&payment_ref)
            .execute(&mut *tx)
            .await;
        let order_id = match inserted {
            Ok(r) => r.last_insert_id(),
            Err(e) if mysql_error(&e) == Some(1452) => return Err(Status::not_found("no such customer")),
            Err(e) if mysql_error(&e) == Some(1062) => {
                // 同じ依頼が同時に来た。先に書いた方の注文を返す
                drop(tx);
                let mut tx = self.pool.begin().await.map_err(internal)?;
                return Self::existing_order(&mut tx, req.customer_id, &req.request_key)
                    .await?
                    .ok_or_else(|| Status::aborted("concurrent order"));
            }
            Err(e) => return Err(internal(e)),
        };
        for (i, (l, price)) in req.lines.iter().zip(&line_prices).enumerate() {
            let line_no = u16::try_from(i + 1).map_err(|_| Status::invalid_argument("too many lines"))?;
            sqlx::query(queries::ORDER_ITEM_INSERT)
                .bind(order_id)
                .bind(line_no)
                .bind(l.product_id)
                .bind(l.quantity)
                .bind(price)
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
        }
        for (&id, &want) in &wanted {
            let r = sqlx::query(queries::PRODUCT_TAKE_STOCK)
                .bind(want)
                .bind(id)
                .bind(want)
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
            if r.rows_affected() != 1 {
                return Err(Status::aborted("stock changed"));
            }
        }
        tx.commit().await.map_err(internal)?;
        Ok(order_id)
    }
}

#[tonic::async_trait]
impl Shop for ShopService {
    async fn create_customer(&self, request: Request<CreateCustomerRequest>) -> Result<Response<Customer>, Status> {
        let r = request.into_inner();
        check_len("email", &r.email, 3, 254)?;
        if !r.email.contains('@') {
            return Err(Status::invalid_argument("email must contain @"));
        }
        check_len("password_hash", &r.password_hash, 1, 255)?;
        check_len("full_name", &r.full_name, 1, 100)?;
        check_len("address", &r.address, 1, 500)?;
        let res = sqlx::query(queries::CUSTOMER_INSERT)
            .bind(&r.email)
            .bind(&r.password_hash)
            .bind(&r.full_name)
            .bind(&r.address)
            .execute(&self.pool)
            .await;
        match res {
            Ok(done) => Ok(Response::new(Customer {
                id: done.last_insert_id(),
                email: r.email,
                full_name: r.full_name,
                address: r.address,
            })),
            Err(e) if mysql_error(&e) == Some(1062) => Err(Status::already_exists("the email is already registered")),
            Err(e) => Err(internal(e)),
        }
    }

    async fn get_customer(&self, request: Request<GetCustomerRequest>) -> Result<Response<Customer>, Status> {
        let id = request.into_inner().id;
        let row = sqlx::query(queries::CUSTOMER_BY_ID)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("no such customer"))?;
        Ok(Response::new(customer(&row).map_err(internal)?))
    }

    async fn find_credential(&self, request: Request<FindCredentialRequest>) -> Result<Response<Credential>, Status> {
        let email = request.into_inner().email;
        let row = sqlx::query(queries::CREDENTIAL_BY_EMAIL)
            .bind(&email)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("no such customer"))?;
        Ok(Response::new(Credential {
            customer_id: row.try_get("id").map_err(internal)?,
            password_hash: text(&row, "password_hash").map_err(internal)?,
        }))
    }

    async fn list_products(&self, _: Request<ListProductsRequest>) -> Result<Response<ProductList>, Status> {
        let rows = sqlx::query(queries::PRODUCTS_ALL).fetch_all(&self.pool).await.map_err(internal)?;
        let products = rows.iter().map(product).collect::<Result<_, _>>().map_err(internal)?;
        Ok(Response::new(ProductList { products }))
    }

    async fn get_product(&self, request: Request<GetProductRequest>) -> Result<Response<Product>, Status> {
        let row = sqlx::query(queries::PRODUCT_BY_ID)
            .bind(request.into_inner().id)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("no such product"))?;
        Ok(Response::new(product(&row).map_err(internal)?))
    }

    async fn place_order(&self, request: Request<PlaceOrderRequest>) -> Result<Response<Order>, Status> {
        let r = request.into_inner();
        if !valid_request_key(&r.request_key) {
            return Err(Status::invalid_argument("request_key must be a UUID"));
        }
        if r.lines.is_empty() || r.lines.len() > MAX_LINES {
            return Err(Status::invalid_argument(format!("an order has 1..={MAX_LINES} lines")));
        }
        if let Some(l) = r.lines.iter().find(|l| !proxy_verified::valid_quantity(l.quantity)) {
            return Err(Status::invalid_argument(format!(
                "quantity must be 1..={} (got {})",
                proxy_verified::MAX_QUANTITY,
                l.quantity
            )));
        }
        check_len("card_token", &r.card_token, 1, 64)?;
        let id = self.place(&r).await?;
        Ok(Response::new(self.load_order(id, r.customer_id).await?))
    }

    async fn list_orders(&self, request: Request<ListOrdersRequest>) -> Result<Response<OrderList>, Status> {
        let customer_id = request.into_inner().customer_id;
        let rows =
            sqlx::query(queries::ORDERS_BY_CUSTOMER).bind(customer_id).fetch_all(&self.pool).await.map_err(internal)?;
        let mut orders = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut o = order(r).map_err(internal)?;
            o.items = self.items(o.id).await?;
            orders.push(o);
        }
        Ok(Response::new(OrderList { orders }))
    }

    async fn get_order(&self, request: Request<GetOrderRequest>) -> Result<Response<Order>, Status> {
        let r = request.into_inner();
        Ok(Response::new(self.load_order(r.order_id, r.customer_id).await?))
    }
}
