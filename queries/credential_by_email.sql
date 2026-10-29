-- ログインの照合材料。
SELECT id, password_hash FROM customers WHERE email = ?
