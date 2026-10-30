-- 顧客自身の注文。決済が拒否された注文は見せない。
SELECT id, total, status, DATE_FORMAT(created_at, '%Y-%m-%d %H:%i:%s.%f') AS created_at
FROM orders WHERE id = ? AND customer_id = ? AND status <> 'declined'
