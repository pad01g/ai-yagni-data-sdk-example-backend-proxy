-- backend-proxy の DB ユーザー（'proxy'@'%'）の権限。固定クエリ（queries/*.sql）が使う分だけを、表と列の単位で許す。
-- DELETE と DDL はできない。価格・合計・単価・パスワードのハッシュは、書いた後は書き換えられない。
-- ユーザーを作るのは DB を持つ側（backend-rust の compose、このリポジトリのテスト）で、このファイルは権限だけを持つ。
-- 固定クエリを足すときは、ここも合わせて直す（テストは proxy をこのユーザーでつなぐ）。
GRANT SELECT ON shop.customers TO 'proxy'@'%';
GRANT INSERT (email, password_hash, full_name, address) ON shop.customers TO 'proxy'@'%';
GRANT UPDATE (session_epoch) ON shop.customers TO 'proxy'@'%';

GRANT SELECT ON shop.products TO 'proxy'@'%';
GRANT UPDATE (stock) ON shop.products TO 'proxy'@'%';

GRANT SELECT ON shop.orders TO 'proxy'@'%';
GRANT INSERT (customer_id, request_key, total, status) ON shop.orders TO 'proxy'@'%';
GRANT UPDATE (status, payment_ref) ON shop.orders TO 'proxy'@'%';

GRANT SELECT, INSERT ON shop.order_items TO 'proxy'@'%';
