-- 決済が通った注文を paid にする。pending でなければ 0 行（ほかの依頼が先に決着させた）。
UPDATE orders SET status = 'paid', payment_ref = ? WHERE id = ? AND status = 'pending'
