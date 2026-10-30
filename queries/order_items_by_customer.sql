-- 顧客の注文のうち、id が ? 以上のものの明細（orders_by_customer と同じトランザクションで読む）。
SELECT i.order_id, i.line_no, i.product_id, p.name AS product_name, i.quantity, i.unit_price
FROM order_items i
JOIN orders o ON o.id = i.order_id
JOIN products p ON p.id = i.product_id
WHERE o.customer_id = ? AND o.id >= ?
ORDER BY i.order_id, i.line_no
