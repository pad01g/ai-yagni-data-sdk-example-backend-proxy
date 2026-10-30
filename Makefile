# ホストには docker 以外何も入れない。CI（GitHub Actions）も同じターゲットを走らせる。
DEV      := ai-yagni-data-rust-dev
VERUS    := ai-yagni-data-verus
NAME     := ai-yagni-data-sdk-example-backend-proxy
CACHE    := -v ai-yagni-data-cargo:/usr/local/cargo/registry -v ai-yagni-data-cargo-git:/usr/local/cargo/git -v $(NAME)-target:/work/target
RUN      := docker run --rm $(CACHE) -v $(CURDIR):/work -w /work $(DEV)

.DEFAULT_GOAL := help
.PHONY: help image verus-image build test lint fmt verify ci sh

help: ## このヘルプ
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

image: ## ビルドとテストのイメージを作る
	docker build -q -t $(DEV) docker/dev >/dev/null

verus-image: ## Verus のイメージを作る（amd64）
	docker build -q --platform linux/amd64 -t $(VERUS) docker/verus >/dev/null

build: image ## ビルド
	$(RUN) cargo build --all-targets --locked

NET      := $(NAME)-test
TESTDB   := $(NAME)-mysql

test: image ## テスト（MySQL のコンテナを試験用のスキーマと grants.sql の権限で立て、proxy のユーザーでつなぐ）
	docker network create $(NET) >/dev/null 2>&1 || true
	docker rm -f $(TESTDB) >/dev/null 2>&1 || true
	docker run -d --rm --name $(TESTDB) --network $(NET) --tmpfs /var/lib/mysql \
	  -e MYSQL_ROOT_PASSWORD=test -e MYSQL_DATABASE=shop -v $(CURDIR)/testdata:/docker-entrypoint-initdb.d:ro \
	  -v $(CURDIR)/grants.sql:/grants.sql:ro \
	  mysql:8.4.6 --character-set-server=utf8mb4 --collation-server=utf8mb4_0900_bin >/dev/null
	docker run --rm $(CACHE) -v $(CURDIR):/work -w /work --network $(NET) \
	  -e DATABASE_URL=mysql://proxy:test@$(TESTDB):3306/shop $(DEV) cargo test --locked; \
	  s=$$?; docker rm -f $(TESTDB) >/dev/null; docker network rm $(NET) >/dev/null; exit $$s

lint: image ## clippy と rustfmt
	$(RUN) sh -c 'cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings'

fmt: image ## 整形する
	$(RUN) cargo fmt --all

verify: verus-image ## Verus で証明を確かめる（verified/）
	docker run --rm --platform linux/amd64 -v ai-yagni-data-verus-cargo:/opt/cargo/registry -v ai-yagni-data-verus-git:/opt/cargo/git \
	  -v $(CURDIR):/work -w /work/verified $(VERUS) cargo verus verify --target-dir /tmp/verus-target

ci: lint test verify ## CI で走らせるもの全部

sh: image ## 開発用のシェル
	docker run --rm -it $(CACHE) -v $(CURDIR):/work -w /work $(DEV) bash

