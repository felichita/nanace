.PHONY: deps deps-down fmt fmt-check lint test test-integration test-model check run

deps: ## Start PostgreSQL and Qdrant
	docker compose up -d --wait

deps-down: ## Stop dependencies (keeps volumes)
	docker compose down

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test: ## Unit tests; no external services
	cargo test --workspace

test-integration: deps ## API + RLS tests against compose services
	cargo test -p nanace-server -- --ignored

test-model: ## Real bge-small-en-v1.5 test; downloads the model on first run
	cargo test -p nanace-embed -- --ignored

check: fmt-check lint test ## Fast local gate

run: deps ## Run the API with .env
	set -a && . ./.env && set +a && cargo run -p nanace-server
