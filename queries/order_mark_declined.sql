-- 決済が拒否された注文を declined にする。pending でなければ 0 行。
UPDATE orders SET status = 'declined' WHERE id = ? AND status = 'pending'
