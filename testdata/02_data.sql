-- proxy のテスト用のデータ。テストは並行に走るので、在庫を数えるテストはそれぞれ専用の商品を使う。
-- docker の初期化スクリプトは既定で latin1 として読まれるので、文字コードを明示する。
SET NAMES utf8mb4;

INSERT INTO products (id, code, name, description, price, stock) VALUES
  (1, 'TSHIRT-001', 'T シャツ', '綿 100% の白い T シャツ。', 2500, 20),
  (2, 'MUG-001', 'マグカップ', '350ml の陶器のマグカップ。', 1200, 8),
  (4, 'STICKER-01', 'ステッカー', '耐水性のロゴステッカー。', 300, 0),
  (10, 'TEST-A', '試験用 A', '冪等のテスト用', 1000, 100),
  (11, 'TEST-B', '試験用 B', '決済の拒否のテスト用', 700, 100),
  (12, 'TEST-C', '試験用 C', '重複した明細のテスト用', 250, 100),
  (13, 'TEST-D', '試験用 D', '決済サービスに届かないときのテスト用', 400, 100),
  (14, 'TEST-E', '試験用 E', '同時の注文のテスト用', 150, 100),
  (15, 'TEST-F', '試験用 F', '決済の拒否で在庫を戻すテスト用', 600, 100);

-- 突き合わせ（reconcile）のテスト用: 決着しないまま古くなった注文。ほかのテストの新しい注文には触らないよう、
-- テストは「1 時間より古い」ものだけを片付ける。
-- 1001 は決済サービスに決済の記録がある（paid になる）、1002 は記録がない（declined になり在庫が戻る）。
INSERT INTO products (id, code, name, description, price, stock) VALUES
  (16, 'TEST-G', '試験用 G', '突き合わせのテスト用', 500, 10);
INSERT INTO customers (id, email, password_hash, full_name, address) VALUES
  (1000, 'stale@example.com', 'x', '古い 注文', '東京都');
INSERT INTO orders (id, customer_id, request_key, total, status, payment_key, created_at) VALUES
  (1001, 1000, '00000000-0000-4000-8000-000000001001', 500, 'pending', '11111111-1111-4111-8111-111111111001', '2026-01-01 00:00:00'),
  (1002, 1000, '00000000-0000-4000-8000-000000001002', 1000, 'pending', '11111111-1111-4111-8111-111111111002', '2026-01-01 00:00:00');
INSERT INTO order_items (order_id, line_no, product_id, quantity, unit_price) VALUES
  (1001, 1, 16, 1, 500),
  (1002, 1, 16, 2, 500);
