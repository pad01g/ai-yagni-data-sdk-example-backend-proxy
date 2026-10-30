-- 顧客の新しい注文から 100 件。決済が拒否された注文は見せない。
SELECT id, total, status, DATE_FORMAT(created_at, '%Y-%m-%d %H:%i:%s.%f') AS created_at
FROM orders WHERE customer_id = ? AND status <> 'declined' ORDER BY id DESC LIMIT 100
