#!/bin/bash
set -eo pipefail
cd "$(dirname "$0")"

MODEL="${1:-parakeet-tdt-0.6b-v3}"
MODEL_DIR="$HOME/.yogurt/parakeet/$MODEL"
BASE_URL="https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main"

if [ "$MODEL" != "parakeet-tdt-0.6b-v3" ]; then
  echo "Only parakeet-tdt-0.6b-v3 is currently supported by this setup script." >&2
  exit 1
fi

echo "==> Setting up Parakeet TDT v3 (ONNX export)"
mkdir -p "$MODEL_DIR"

download() {
  local name="$1"
  if [ -f "$MODEL_DIR/$name" ]; then
    echo "==> Already present: $MODEL_DIR/$name"
  else
    echo "==> Downloading $name ..."
    curl -fL --progress-bar "$BASE_URL/$name?download=true" -o "$MODEL_DIR/$name"
  fi
}

download encoder-model.onnx
download encoder-model.onnx.data
download decoder_joint-model.onnx
download vocab.txt

echo "==> Building yogurt with Parakeet support..."
make build FEATURES=parakeet

echo ""
echo "Done! Run with:"
echo "  STT_MODEL=parakeet/v3 ./yogurt"
