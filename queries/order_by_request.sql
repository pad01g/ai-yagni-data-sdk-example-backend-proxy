-- 依頼の冪等キーで、既にある注文を探す。
SELECT id, total, status FROM orders WHERE customer_id = ? AND request_key = ?
