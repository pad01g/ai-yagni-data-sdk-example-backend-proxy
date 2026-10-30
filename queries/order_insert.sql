-- 在庫を引き当てた注文を、決済の前に pending で書く。
INSERT INTO orders (customer_id, request_key, total, status) VALUES (?, ?, ?, 'pending')
