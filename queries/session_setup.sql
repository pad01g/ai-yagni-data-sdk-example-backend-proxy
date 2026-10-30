-- 接続ごとに 1 度流す。行の固定を待つのは 5 秒まで（既定の 50 秒も待たせず、混んでいると早く返す）。
SET SESSION innodb_lock_wait_timeout = 5
