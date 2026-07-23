MODEL ?= base
# Ad-hoc by default; set to a stable identity (e.g. "yogurt-dev") so mic
# permission survives rebuilds: make build SIGN_ID=yogurt-dev
SIGN_ID ?= -

.DEFAULT_GOAL := help

.PHONY: help setup build run clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-8s\033[0m %s\n", $$1, $$2}'

setup: ## Install whisper-cpp, download the model, and build with whisper support (MODEL=base)
	./setup-whisper.sh $(MODEL)

build: ## Build yogurt.app bundle (required for mic permission) + ./yogurt symlink
	mkdir -p yogurt.app/Contents/MacOS
	cp Info.plist yogurt.app/Contents/Info.plist
	go build -o yogurt.app/Contents/MacOS/yogurt .
	codesign -f -s "$(SIGN_ID)" yogurt.app
	ln -sf yogurt.app/Contents/MacOS/yogurt yogurt

run: build ## Build and run (pass flags via ARGS="--list-devices")
	./yogurt $(ARGS)

clean: ## Remove built binaries
	rm -f yogurt yogurtgo
