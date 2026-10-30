-- 決済が拒否された注文の在庫を戻す。同じ商品が複数の明細にあれば足し合わせてから戻す
-- （複数の表の UPDATE は、1 つの行を 1 度しか更新しないため）。
UPDATE products p
JOIN (SELECT product_id, SUM(quantity) AS quantity FROM order_items WHERE order_id = ? GROUP BY product_id) i
  ON i.product_id = p.id
SET p.stock = p.stock + i.quantity
