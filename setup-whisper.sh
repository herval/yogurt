#!/bin/bash
set -eo pipefail
cd "$(dirname "$0")"

MODEL="${1:-base}"
MODEL_DIR="$HOME/.yogurt/whisper"
MODEL_FILE="$MODEL_DIR/ggml-${MODEL}.bin"
MODEL_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-${MODEL}.bin"

echo "==> Setting up Whisper (model: $MODEL)"

# Download model if not already present
if [ -f "$MODEL_FILE" ]; then
  echo "==> Model already at $MODEL_FILE"
else
  mkdir -p "$MODEL_DIR"
  echo "==> Downloading ggml-${MODEL}.bin ..."
  curl -L --progress-bar "$MODEL_URL" -o "$MODEL_FILE"
fi

# Install whisper-cpp via homebrew (handles compilation, headers, and libs)
echo "==> Installing whisper-cpp via homebrew..."
brew install whisper-cpp

WHISPER_PREFIX=$(brew --prefix whisper-cpp)
echo "==> whisper-cpp at: $WHISPER_PREFIX"

# Pull Go bindings (adds to go.mod)
echo "==> Fetching whisper.cpp Go bindings..."
go get github.com/ggerganov/whisper.cpp/bindings/go

# Brew ships libwhisper.dylib with ggml baked in, but the Go bindings still
# request -lggml -lggml-base -lggml-cpu etc. via #cgo LDFLAGS.
# Create empty stub archives to satisfy the linker (symbols are in libwhisper).
STUB_DIR=$(mktemp -d)
trap 'rm -rf "$STUB_DIR"' EXIT
printf 'void _yogurt_ggml_stub(void) {}\n' > "$STUB_DIR/stub.c"
cc -c "$STUB_DIR/stub.c" -o "$STUB_DIR/stub.o"
for lib in ggml ggml-base ggml-cpu ggml-metal ggml-blas; do
  ar rcs "$STUB_DIR/lib${lib}.a" "$STUB_DIR/stub.o"
done

# Build yogurt with whisper support
echo "==> Building yogurt with whisper support..."
CGO_CFLAGS="-I${WHISPER_PREFIX}/include" \
CGO_LDFLAGS="-L${WHISPER_PREFIX}/lib -L${STUB_DIR} -lwhisper -lstdc++ -framework Accelerate -framework Foundation" \
go build -tags whisper -o yogurt .

echo ""
echo "Done! Run with:"
echo "  STT_MODEL=whisper/$MODEL ./yogurt"
