-- ログアウト: 世代を上げて、その顧客のそれまでのセッションをすべて無効にする。
UPDATE customers SET session_epoch = session_epoch + 1 WHERE id = ?
