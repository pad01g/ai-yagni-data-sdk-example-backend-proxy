-- ログインの照合材料（照合は proxy がする。ハッシュを proxy の外へは出さない）。
SELECT id, password_hash, session_epoch FROM customers WHERE email = ?
