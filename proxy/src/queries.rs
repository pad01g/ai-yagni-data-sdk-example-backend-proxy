//! 固定クエリ。proxy が DB に投げる SQL はここにあるものだけで、どれもビルド時に `queries/*.sql` から埋め込む。
//! 値は `?` に束縛する（SQL の文字列を組み立てない）。
//! backend-rust の CI は、`queries/` のどのファイルも今のスキーマで PREPARE できることを確かめる。
pub const CUSTOMER_INSERT: &str = include_str!("../../queries/customer_insert.sql");
pub const CUSTOMER_BY_ID: &str = include_str!("../../queries/customer_by_id.sql");
pub const CREDENTIAL_BY_EMAIL: &str = include_str!("../../queries/credential_by_email.sql");
pub const PRODUCTS_ALL: &str = include_str!("../../queries/products_all.sql");
pub const PRODUCT_BY_ID: &str = include_str!("../../queries/product_by_id.sql");
pub const PRODUCT_LOCK: &str = include_str!("../../queries/product_lock.sql");
pub const PRODUCT_TAKE_STOCK: &str = include_str!("../../queries/product_take_stock.sql");
pub const ORDER_BY_REQUEST: &str = include_str!("../../queries/order_by_request.sql");
pub const ORDER_INSERT: &str = include_str!("../../queries/order_insert.sql");
pub const ORDER_ITEM_INSERT: &str = include_str!("../../queries/order_item_insert.sql");
pub const ORDER_BY_ID: &str = include_str!("../../queries/order_by_id.sql");
pub const ORDERS_BY_CUSTOMER: &str = include_str!("../../queries/orders_by_customer.sql");
pub const ORDER_ITEMS_BY_ORDER: &str = include_str!("../../queries/order_items_by_order.sql");
