SELECT id, customer_id, total, status, payment_ref, DATE_FORMAT(created_at, '%Y-%m-%d %H:%i:%s.%f') AS created_at
FROM orders WHERE id = ? AND customer_id = ?
