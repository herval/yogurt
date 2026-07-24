MODEL ?= base
# A stable signing identity so the microphone grant survives rebuilds; ad-hoc
# ("-") gets a new code identity every build, so macOS re-prompts each time.
# yogurt-dev is a local self-signed code-signing cert in the login keychain.
SIGN_ID ?= yogurt-dev
# Extra cargo features, e.g. FEATURES=whisper
FEATURES ?=

CARGO_FLAGS = --release $(if $(FEATURES),--features $(FEATURES))

.DEFAULT_GOAL := help

.PHONY: help setup build run clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-8s\033[0m %s\n", $$1, $$2}'

setup: ## Download the whisper model and build with whisper support (MODEL=base)
	./setup-whisper.sh $(MODEL)

build: ## Build yogurt.app bundle (required for mic permission) + ./yogurt symlink
	@if pgrep -qf "yogurt.app/Contents/MacOS/yogurt"; then \
		echo "ERROR: yogurt is running — quit it first (replacing the binary kills it)"; exit 1; \
	fi
	cargo build $(CARGO_FLAGS)
	mkdir -p yogurt.app/Contents/MacOS
	cp Info.plist yogurt.app/Contents/Info.plist
	cp target/release/yogurt yogurt.app/Contents/MacOS/yogurt
	codesign -f -s "$(SIGN_ID)" yogurt.app
	ln -sf yogurt.app/Contents/MacOS/yogurt yogurt

run: build ## Build and run (pass flags via ARGS="--list-devices")
	./yogurt $(ARGS)

clean: ## Remove build artifacts
	cargo clean
	rm -rf yogurt.app yogurt
