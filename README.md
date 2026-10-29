# ai-yagni-data-sdk-example-backend-proxy

EC の例（[ai-yagni-data-sdk-example-backend-rust](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-rust)）で使う、固定クエリだけを持つ SQL のマイクロサービスです（gRPC）。

backend-rust がつながる外部のサービスは、すべてこの proxy を通ります。

- **DB と決済への接続**: MySQL への接続と、決済サービス（[payment](https://github.com/pad01g/ai-yagni-data-sdk-example-payment)）への接続は、proxy だけが持ちます。
- **受け付けるもの**: proxy が受け付けるのは、[`shop.proto`](api/proto/shop/v1/shop.proto) の操作だけです。SQL の文字列や、表や列の名前を受け取る操作はありません。
- **DB に投げるもの**: DB に投げる SQL は、[`queries/*.sql`](queries) の固定クエリだけです。
  - どのクエリもビルド時に埋め込みます（[`queries.rs`](proxy/src/queries.rs)）。
  - 値は `?` に束縛します。
- **効果**: クエリを固定するという djb 流のやり方に沿っています。backend-rust が乗っ取られても、固定された操作以外の読み書きはできません。
  - 例: ほかの表を読む、価格を書き換える、決済を飛ばして注文を書く。

```
frontend ──HTTP──▶ backend-rust ──gRPC──▶ backend-proxy ──SQL──▶ MySQL
                                                 └──────HTTP──▶ payment
```

## 操作

| 操作 | 固定クエリ |
|---|---|
| `CreateCustomer` | `customer_insert` |
| `GetCustomer` | `customer_by_id` |
| `FindCredential` | `credential_by_email`（照合は backend-rust が argon2 でする） |
| `ListProducts` / `GetProduct` | `products_all` / `product_by_id` |
| `PlaceOrder` | `order_by_request`, `product_lock`, `order_insert`, `order_item_insert`, `product_take_stock` |
| `ListOrders` / `GetOrder` | `orders_by_customer` / `order_by_id`, `order_items_by_order` |

`PlaceOrder` は次の 5 つを、1 つのトランザクションでします。

1. 商品の行を固定する。
2. 在庫を引き当てる。
3. DB の価格から合計を計算する。
4. 決済サービスに請求する。
5. 注文と明細を書く。

- **値の決め方**: 金額と在庫は、backend-rust の言う値を使わず、DB の値から proxy が決めます。
- **冪等**: 同じ `(customer_id, request_key)` には同じ注文を返します。決済サービスにも同じ冪等キーで請求するので、やり直しても二重に請求しません。
- **境界の限界**: `customer_id` は backend-rust がセッションから渡します。そのため、backend-rust が乗っ取られた場合、ほかの顧客の注文を読むことまでは防げません。この境界が防ぐのは「操作の種類」です。

## スキーマとの関係

スキーマの正本（Atlas が当てる `schema.sql`）は backend-rust にあります。

- **backend-rust の CI**: この proxy を submodule として持ち、`queries/` のどのファイルも正本のスキーマで `PREPARE` できることを確かめます（データの未来の検査の一部）。
- **この proxy の単独のテスト**: 正本の写し（[`testdata/`](testdata)）を使います。

## Verus

[`verified/`](verified/src/lib.rs) は、注文の金額と在庫の判断を証明しています。

| 関数 | 証明していること |
|---|---|
| `valid_quantity` | 数量は 1 以上 99 以下 |
| `reserve` | 在庫が足りるときだけ引き当て、残りを返す |
| `order_total` | 単価 × 数量の和が仕様どおり。あふれるなら None |

## 動かし方（ホストには docker 以外何も入れない）

```sh
make test     # MySQL を試験用のスキーマで立て、proxy を gRPC で呼ぶ（決済はテストの中の偽物）
make lint     # rustfmt と clippy
make verify   # Verus（amd64 のイメージ）
```

環境変数は `DATABASE_URL`（`mysql://...`）、`PAYMENT_URL`（`http://payment:8080`）、`LISTEN`（既定 `0.0.0.0:50051`）です。

## 使い方（Cargo、クライアント）

```toml
[dependencies]
shop-proxy-api = { git = "https://github.com/pad01g/ai-yagni-data-sdk-example-backend-proxy", tag = "v0.1.0" }
```

## ライセンス

MIT
