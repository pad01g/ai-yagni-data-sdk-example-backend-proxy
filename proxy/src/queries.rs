//! 固定クエリ。proxy が DB に投げる SQL はここにあるものだけで、どれもビルド時に `queries/*.sql` から埋め込む。
//! 値は `?` に束縛する（SQL の文字列を組み立てない）。
//! backend-rust の CI は、`queries/` のどのファイルも今のスキーマで PREPARE できることを確かめる。
macro_rules! queries {
    ($($name:ident = $file:literal;)*) => {
        $(pub const $name: &str = include_str!(concat!("../../queries/", $file));)*
        /// すべての固定クエリ（テストで、ファイルとの対応を確かめる）。
        pub const ALL: &[(&str, &str)] = &[$(($file, $name)),*];
    };
}

queries! {
    SESSION_SETUP = "session_setup.sql";
    CUSTOMER_INSERT = "customer_insert.sql";
    CUSTOMER_BY_ID = "customer_by_id.sql";
    CUSTOMER_SESSION = "customer_session.sql";
    CUSTOMER_END_SESSIONS = "customer_end_sessions.sql";
    CREDENTIAL_BY_EMAIL = "credential_by_email.sql";
    PRODUCTS_ALL = "products_all.sql";
    PRODUCT_LOCK = "product_lock.sql";
    PRODUCT_SET_STOCK = "product_set_stock.sql";
    PRODUCT_LOCK_ORDER = "product_lock_order.sql";
    PRODUCT_RESTOCK_ORDER = "product_restock_order.sql";
    ORDER_BY_REQUEST = "order_by_request.sql";
    ORDER_INSERT = "order_insert.sql";
    ORDER_ITEM_INSERT = "order_item_insert.sql";
    ORDER_MARK_PAID = "order_mark_paid.sql";
    ORDER_MARK_DECLINED = "order_mark_declined.sql";
    ORDERS_PENDING_STALE = "orders_pending_stale.sql";
    ORDER_BY_ID = "order_by_id.sql";
    ORDERS_BY_CUSTOMER = "orders_by_customer.sql";
    ORDER_ITEMS_BY_ORDER = "order_items_by_order.sql";
    ORDER_ITEMS_BY_CUSTOMER = "order_items_by_customer.sql";
}
