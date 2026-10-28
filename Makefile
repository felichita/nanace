.PHONY: deps deps-down fmt fmt-check lint test test-integration test-model check run desktop-deps desktop-dev desktop-build desktop-check

DESKTOP := apps/desktop

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

test-integration: deps ## API, RLS and MCP tests against compose services
	# qdrant-test keeps data in memory: start it empty and stop it afterwards
	# (even when tests fail) so test collections do not hold RAM.
	docker compose restart qdrant-test
	until curl -sf http://127.0.0.1:6335/readyz >/dev/null; do sleep 0.5; done
	cargo test -p nanace-server -p nanace-mcp -- --ignored; status=$$?; docker compose stop qdrant-test; exit $$status

test-model: ## Real bge-small-en-v1.5 test; downloads the model on first run
	cargo test -p nanace-embed -- --ignored

check: fmt-check lint test ## Fast local gate

desktop-deps: ## npm packages and the bundled MCP server (Tauri sidecar)
	cd $(DESKTOP) && npm ci && npm run mcp

desktop-dev: desktop-deps ## Run the desktop app against the API
	cd $(DESKTOP) && npx tauri dev

desktop-build: desktop-deps ## Build the desktop app bundle
	cd $(DESKTOP) && npx tauri build

desktop-check: desktop-deps ## Desktop gate: types, UI tests, Rust shell
	cd $(DESKTOP) && npm run check
	cd $(DESKTOP)/src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test

run: deps ## Run the API with .env
	set -a && . ./.env && set +a && cargo run -p nanace-server
