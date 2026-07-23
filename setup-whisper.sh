#!/bin/bash
set -eo pipefail
cd "$(dirname "$0")"

MODEL="${1:-base}"
MODEL_DIR="$HOME/.yogurt/whisper"
MODEL_FILE="$MODEL_DIR/ggml-${MODEL}.bin"
MODEL_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-${MODEL}.bin"

echo "==> Setting up Whisper (model: $MODEL)"

if ! command -v cmake >/dev/null; then
  echo "==> cmake is required to build whisper.cpp (brew install cmake)" >&2
  exit 1
fi

if [ -f "$MODEL_FILE" ]; then
  echo "==> Model already at $MODEL_FILE"
else
  mkdir -p "$MODEL_DIR"
  echo "==> Downloading ggml-${MODEL}.bin ..."
  curl -L --progress-bar "$MODEL_URL" -o "$MODEL_FILE"
fi

# whisper-rs vendors and builds whisper.cpp itself; no brew packages needed.
echo "==> Building yogurt with whisper support..."
make build FEATURES=whisper

echo ""
echo "Done! Run with:"
echo "  STT_MODEL=whisper/$MODEL ./yogurt"
