-- 在庫を引く。在庫が足りなければ 0 行（呼ぶ側が失敗にする）。
UPDATE products SET stock = stock - ? WHERE id = ? AND stock >= ?
