-- セッションの世代。セッションの世代が今の世代と違えば、そのセッションは無効（ログアウトした後）。
SELECT session_epoch FROM customers WHERE id = ?
