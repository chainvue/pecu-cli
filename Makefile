# `just` isn't installed on this machine, so the task runner is plain make.
# Nothing here needs a network connection except the `testnet` target.

.DEFAULT_GOAL := help
.PHONY: help check fmt fmt-check lint test testnet snapshots gallery build run clean site serve check-site

help: ## Show this help
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | awk -F':.*?## ' '{printf "  \033[32m%-12s\033[0m %s\n", $$1, $$2}'

check: fmt-check lint test build ## Everything CI would run, offline

fmt: ## Format the source tree
	cargo fmt --all

fmt-check: ## Fail if the tree is unformatted
	cargo fmt --all -- --check

# Clippy gets its own target directory on purpose. It swaps the rustc wrapper, so
# sharing target/ with build and run refingerprints every crate -- and with the
# SDK in the graph, switching between `make check` and `cargo run` costs a full
# ~70s rebuild each way. Separate directories cost disk and nothing else.
lint: ## Clippy, warnings are errors
	CARGO_TARGET_DIR=target/lint cargo clippy --all-targets -- -D warnings

test: ## Offline tests only — must pass with the network unplugged
	cargo test

testnet: ## Tests that talk to api.verustest.net
	cargo test -- --ignored

snapshots: ## Accept the current UI output as the new snapshots
	INSTA_UPDATE=always cargo test --test ui

gallery: ## Eyeball the UI kit in the phosphor skin
	cargo run -q -- dev ui --theme phosphor

build: ## Debug build
	cargo build

run: ## Run the CLI: make run ARGS="doctor"
	cargo run -- $(ARGS)

# --- website -----------------------------------------------------------------
# The site is docs/*.md plus web/. Python-Markdown is the only dependency and it
# lives in a venv under web/, so nothing is installed system-wide and nothing
# here touches the Rust build.
WEB_PY := web/.venv/bin/python

$(WEB_PY):
	python3 -m venv web/.venv
	web/.venv/bin/pip install --quiet --disable-pip-version-check markdown==3.7

site: $(WEB_PY) ## Build the website into web/_site
	$(WEB_PY) web/build.py

PORT ?= 8000
check-site: site ## Assert the site has no sideways scroll and a working Replay (needs Chrome)
	web/check-site.sh

serve: $(WEB_PY) ## Build the website and serve it: make serve [PORT=8000]
	$(WEB_PY) web/build.py --serve --port $(PORT)

clean:
	cargo clean
	rm -rf web/_site
