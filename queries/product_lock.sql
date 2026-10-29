-- 注文のトランザクションの中で、商品の行を固定して読む（在庫の引き当てのため）。
SELECT id, name, price, stock FROM products WHERE id = ? FOR UPDATE
