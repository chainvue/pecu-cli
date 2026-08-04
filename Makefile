# `just` isn't installed on this machine, so the task runner is plain make.
# Nothing here needs a network connection except the `testnet` target.

.DEFAULT_GOAL := help
.PHONY: help check fmt fmt-check lint test testnet build run clean

help: ## Show this help
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | awk -F':.*?## ' '{printf "  \033[32m%-12s\033[0m %s\n", $$1, $$2}'

check: fmt-check lint test build ## Everything CI would run, offline

fmt: ## Format the source tree
	cargo fmt --all

fmt-check: ## Fail if the tree is unformatted
	cargo fmt --all -- --check

lint: ## Clippy, warnings are errors
	cargo clippy --all-targets -- -D warnings

test: ## Offline tests only — must pass with the network unplugged
	cargo test

testnet: ## Tests that talk to api.verustest.net
	cargo test -- --ignored

build: ## Debug build
	cargo build

run: ## Run the CLI: make run ARGS="doctor"
	cargo run -- $(ARGS)

clean:
	cargo clean
