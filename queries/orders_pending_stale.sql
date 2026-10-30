-- 決着しないまま ? 秒より古くなった注文（突き合わせて片付ける。古い順に 50 件ずつ）。
SELECT id, customer_id, total, status, payment_key FROM orders
WHERE status = 'pending' AND created_at < NOW(3) - INTERVAL ? SECOND
ORDER BY id LIMIT 50
