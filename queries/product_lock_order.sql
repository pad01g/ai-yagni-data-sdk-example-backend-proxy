-- 在庫を戻す前に、注文の商品の行を ID の順に固定する（引き当てと同じ順にして、デッドロックを避ける）。
SELECT id FROM products
WHERE id IN (SELECT product_id FROM order_items WHERE order_id = ?)
ORDER BY id FOR UPDATE
