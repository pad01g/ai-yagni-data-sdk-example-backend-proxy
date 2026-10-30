SELECT i.order_id, i.line_no, i.product_id, p.name AS product_name, i.quantity, i.unit_price
FROM order_items i JOIN products p ON p.id = i.product_id
WHERE i.order_id = ? ORDER BY i.line_no
