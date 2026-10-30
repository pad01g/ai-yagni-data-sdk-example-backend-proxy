-- テストの proxy がつなぐユーザー。権限は grants.sql（make test が /grants.sql に置く）。
CREATE USER 'proxy'@'%' IDENTIFIED BY 'test';
SOURCE /grants.sql;
