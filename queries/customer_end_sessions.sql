-- ログアウト: 世代を上げて、その顧客のそれまでのセッションをすべて無効にする。
-- 今の世代のトークンでだけ上げる（ログアウト済みの古いトークンで、新しいセッションを終わらせられないように）。
UPDATE customers SET session_epoch = session_epoch + 1 WHERE id = ? AND session_epoch = ?
