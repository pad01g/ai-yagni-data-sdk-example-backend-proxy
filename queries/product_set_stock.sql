-- 注文のトランザクションの中で、固定した行の在庫を引き当てた後の値にする（値は Verus で証明した reserve が決める）。
-- 読んだ時の在庫と違えば 0 行（呼ぶ側が失敗にする）。
UPDATE products SET stock = ? WHERE id = ? AND stock = ?
