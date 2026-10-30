-- 在庫を引き当てた注文を、決済の前に pending で書く。payment_key は決済サービスへの冪等キー（proxy が作る UUID）。
INSERT INTO orders (customer_id, request_key, total, status, payment_key) VALUES (?, ?, ?, 'pending', ?)
