//go:build !whisper

package transcription

import "fmt"

// NewWhisperClient returns an error stub when the binary is not built with -tags whisper.
// To enable Whisper support:
//  1. Install whisper.cpp Go bindings: go get github.com/ggerganov/whisper.cpp/bindings/go
//  2. Download a model, e.g.: ~/.yogurt/whisper/ggml-base.bin
//  3. Build: go build -tags whisper
func NewWhisperClient(sampleRate int, model string) STTClient {
	return &whisperStub{}
}

type whisperStub struct{}

func (w *whisperStub) SetOnSegment(func(Segment))    {}
func (w *whisperStub) SetOnError(fn func(error))      { fn(fmt.Errorf("whisper support not compiled in; rebuild with -tags whisper")) }
func (w *whisperStub) SetOnConnected(func())          {}
func (w *whisperStub) SetOnDisconnect(func())         {}
func (w *whisperStub) Connect() error                 { return fmt.Errorf("whisper support not compiled in; rebuild with -tags whisper") }
func (w *whisperStub) SendAudio([]byte) error         { return nil }
func (w *whisperStub) Close() error                   { return nil }
func (w *whisperStub) IsConnected() bool              { return false }
