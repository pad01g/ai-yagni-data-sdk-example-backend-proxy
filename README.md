# ai-yagni-data-sdk-example-backend-proxy

EC の例（[ai-yagni-data-sdk-example-backend-rust](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-rust)）で使う、固定クエリだけを持つ SQL のマイクロサービスです（gRPC）。

backend-rust がつながる外部のサービスは、すべてこの proxy を通ります。

- **DB と決済への接続**: MySQL への接続と、決済サービス（[payment](https://github.com/pad01g/ai-yagni-data-sdk-example-payment)）への接続は、proxy だけが持ちます。
- **受け付けるもの**: proxy が受け付けるのは、[`shop.proto`](api/proto/shop/v1/shop.proto) の操作だけです。SQL の文字列や、表や列の名前を受け取る操作はありません。
- **DB に投げるもの**: DB に投げる SQL は、[`queries/*.sql`](queries) の固定クエリだけです。
  - どのクエリもビルド時に埋め込みます（[`queries.rs`](proxy/src/queries.rs)）。
  - 値は `?` に束縛します。
- **DB の権限**: proxy の DB ユーザーの権限は [`grants.sql`](grants.sql) にあり、固定クエリが使う表と列だけを許します。
  - DELETE と DDL はできません。
  - 価格・合計・単価・パスワードのハッシュは、書いた後は書き換えられません。
  - テストも、このユーザーでつないで走らせます。
- **認証**: 顧客の認証も proxy がします（[`auth.rs`](proxy/src/auth.rs)）。
  - パスワードの照合（argon2id）と、セッションのトークンの署名は proxy の中だけで行います。署名の鍵（`SESSION_SECRET`）を持つのも proxy だけです。
  - 顧客を名指しする操作は、顧客 ID ではなくトークンを受け取ります。
- **効果**: クエリを固定するという djb 流のやり方に沿っています。backend-rust が乗っ取られても、次のことはできません。
  - 固定された操作以外の読み書き（ほかの表を読む、価格を書き換える、決済を飛ばして注文を書く）。
  - ほかの顧客の個人情報や注文を読むこと、ほかの顧客として注文すること。
  - パスワードのハッシュを読むこと。

```
frontend ──HTTP──▶ backend-rust ──gRPC──▶ backend-proxy ──SQL──▶ MySQL
                                                 └──────HTTP──▶ payment
```

## 操作

| 操作 | 固定クエリ |
|---|---|
| `SignUp` | `customer_insert` |
| `LogIn` | `credential_by_email`, `customer_by_id` |
| `LogOut` | `customer_end_sessions`（今の世代のトークンでだけ上げる） |
| `Me` | `customer_session`, `customer_by_id` |
| `ListProducts` | `products_all` |
| `PlaceOrder` | 下を見てください |
| `ListOrders` | `customer_session`, `orders_by_customer`, `order_items_by_customer` |
| `GetOrder` | `customer_session`, `order_by_id`, `order_items_by_order` |

接続ごとに `session_setup` を 1 度流します（行の固定を待つのは 5 秒まで）。

### セッション

- **形**: トークンは `顧客 ID.世代.発行時刻.署名`（HMAC-SHA256）です。backend-rust は中身を解釈せず、cookie に入れて返すだけです。
- **ログアウト**: 顧客の世代（`customers.session_epoch`）を上げると、その顧客のそれまでのトークンはすべて無効になります。
  - トークンを検証するたびに、今の世代と比べます（`customer_session`）。
- **期限**: 期限（既定 1 日）の判定には、Verus で証明した `session_is_fresh` を使います。
- **ログインの照合**:
  - 登録のないメールでも、照合と同じだけ argon2 を計算します。応答時間で登録の有無が漏れないようにするためです。
  - argon2 を同時に走らせる数は CPU の数までです。空きが 2 秒で出なければ `UNAVAILABLE "busy"` を返します。
- **メール**: 小文字にそろえて保存し、照合します。
- **回数の制限**: proxy のメモリで数えます。nginx の送り元の IP ごとの制限に加えるもので、IP を替えながらの総当たりや、注文の連打を遅くします。
  - 同じメールへのログインの失敗が 15 分に 10 回を超えると、しばらく照合せずに `RESOURCE_EXHAUSTED "too many attempts"` を返します。
  - 注文の依頼は、顧客ごとに 10 分に 30 回までです。
- **古いトークン**: ログアウト済みの古いトークンでログアウトしても、新しいセッションは終わりません。

### 注文（`PlaceOrder`）

決済サービスを待つあいだは、行の固定もトランザクションも持ちません。そのため、注文は 2 段で書きます。

1. **引き当て**（1 つ目のトランザクション）
   1. 商品の行を ID の順に固定します（`product_lock`）。
   2. 同じ商品の明細を足し合わせて在庫を引き当てます（`reserve`、`product_set_stock`）。
   3. DB の価格から合計を決めます（`order_total`）。
   4. 注文を `pending` で書きます（`order_insert`、`order_item_insert`）。
2. **請求**（トランザクションの外）: 決済サービスに請求します。
   - 冪等キーは、注文ごとに proxy が作る乱数の UUID（`orders.payment_key`）です。
   - 注文の ID を使わないのは、DB を作り直して ID が繰り返されても、前の決済と取り違えないためです。
3. **決着**
   - 決済が通れば `paid` にします（`order_mark_paid`）。
   - 拒否なら、1 つのトランザクションで `declined` にし、在庫を戻します（`product_lock_order`、`order_mark_declined`、`product_restock_order`）。
     - 商品の行は、引き当てと同じ ID の順に固定します（デッドロックを避ける）。
   - デッドロックや行の固定の待ちの時間切れなら、その場で 3 度までやり直します。

この形には、次の性質があります。

- **値の決め方**: 金額と在庫は、backend-rust の言う値を使わず、DB の値から proxy が決めます。
- **冪等**: 同じ `(顧客, request_key)` には同じ注文を返します。
  - 同じキーで明細の違う注文は `ALREADY_EXISTS "request_key reused"` です。前の注文を黙って返しません。
  - 同じ依頼が同時に来ても、注文は 1 つです。
- **決済サービスに届かないとき**: `UNAVAILABLE "payment unavailable"` を返し、注文は `pending` のまま、在庫も引き当てたまま残ります。
  - 同じ依頼をやり直すと、同じ冪等キーで請求して決着させます。答えが途中で失われた請求も、二重には請求しません。
  - 前と違うカードでやり直すと、`FAILED_PRECONDITION "payment conflict"` です。
- **中断されないこと**: 呼んだ側が待つのをやめても（切断・時間切れ）、決済と決着は最後まで進めます。
- **突き合わせ**: 決着しないまま古くなった `pending` の注文（既定 10 分、`RECONCILE_AFTER`）は、proxy が 1 分ごとに片付けます（`orders_pending_stale`）。
  - 決済サービスの記録を冪等キーで探し（`GET /charges?idempotency_key=`）、決済が通っていれば `paid` にします。
  - 拒否されていたか、記録がなければ `declined` にして在庫を戻します。
  - 利用者は結果を受け取っていないので、突き合わせで新たに請求することはしません。
  - 突き合わせの直前に利用者がやり直した場合だけは、決済が通っても注文が `declined` になりえます。待つ時間を十分に長くして、その窓を小さくしています。

### 失敗

失敗は gRPC の状態と、決まった短い英語のメッセージで返します（一覧は `shop.proto` の先頭）。

- DB の詳しいエラーや決済サービスの URL は、proxy のログにだけ書きます。呼ぶ側には `INTERNAL "internal"` とだけ返します。
- 決済の拒否の理由は、知っているものだけをそのまま返します。

## スキーマとの関係

スキーマの正本（Atlas が当てる `schema.sql`）は backend-rust にあります。

- **backend-rust の CI**: この proxy を submodule として持ち、次の 2 つを確かめます。
  - `queries/` のどのファイルも、正本のスキーマで `PREPARE` できること（データの未来の検査の一部）。
  - [`testdata/01_schema.sql`](testdata/01_schema.sql) が正本の写しであること。
- **この proxy の単独のテスト**: その写しを使います。

## Verus

[`verified/`](verified/src/lib.rs) は、注文の金額と在庫、セッションの期限の判断を証明しています。

| 関数 | 証明していること |
|---|---|
| `valid_quantity` | 数量は 1 以上 99 以下（明細ごとと、同じ商品の合計の両方に使う） |
| `reserve` | 在庫が足りるときだけ引き当て、残りを返す（その値を DB に書く） |
| `order_total` | 単価 × 数量の和が仕様どおり。あふれるなら None |
| `session_is_fresh` | セッションは発行から期限までだけ有効。未来の時刻に発行されたものは無効 |

## 動かし方（ホストには docker 以外何も入れない）

```sh
make test     # MySQL を試験用のスキーマと grants.sql の権限で立て、proxy を gRPC で呼ぶ（決済はテストの中の偽物）
make lint     # rustfmt と clippy
make verify   # Verus（amd64 のイメージ）
```

環境変数は次のとおりです。

| 変数 | 中身 |
|---|---|
| `DATABASE_URL` | `mysql://proxy:...@mysql:3306/shop` |
| `PAYMENT_URL` | `http://payment:8080`（1 回の請求を待つのは 5 秒まで） |
| `SESSION_SECRET` | セッションの署名の鍵。32 バイト以上でなければ起動しない |
| `SESSION_TTL` | セッションの有効な秒数（既定 86400） |
| `RECONCILE_AFTER` | 何秒たった `pending` の注文を突き合わせるか（既定 600） |
| `LISTEN` | 既定 `0.0.0.0:50051` |

## 使い方（Cargo、クライアント）

```toml
[dependencies]
shop-proxy-api = { git = "https://github.com/pad01g/ai-yagni-data-sdk-example-backend-proxy", tag = "v0.3.0" }
```

## ライセンス

MIT
