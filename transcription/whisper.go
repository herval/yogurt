//go:build whisper

package transcription

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	whisper "github.com/ggerganov/whisper.cpp/bindings/go/pkg/whisper"
)

// WhisperClient is a local batch STT adapter using whisper.cpp CGo bindings.
// Audio is buffered during recording; transcription runs when Close() is called.
//
// Model path resolution for STT_MODEL=whisper/<name>:
//   - "base"  → ~/.yogurt/whisper/ggml-base.bin
//   - "tiny"  → ~/.yogurt/whisper/ggml-tiny.bin
//   - "path:/absolute/path.bin" → explicit path
type WhisperClient struct {
	sampleRate int
	modelPath  string

	mu       sync.Mutex
	audioBuf []byte
	closed   bool

	onSegment    func(Segment)
	onError      func(error)
	onConnected  func()
	onDisconnect func()
}

func NewWhisperClient(sampleRate int, model string) STTClient {
	return &WhisperClient{
		sampleRate: sampleRate,
		modelPath:  resolveWhisperModel(model),
	}
}

func (c *WhisperClient) SetOnSegment(fn func(Segment))    { c.onSegment = fn }
func (c *WhisperClient) SetOnError(fn func(error))         { c.onError = fn }
func (c *WhisperClient) SetOnConnected(fn func())           { c.onConnected = fn }
func (c *WhisperClient) SetOnDisconnect(fn func())          { c.onDisconnect = fn }

func (c *WhisperClient) Connect() error {
	if _, err := os.Stat(c.modelPath); err != nil {
		return fmt.Errorf("whisper model not found at %s — download a model to that path", c.modelPath)
	}
	if c.onConnected != nil {
		go c.onConnected()
	}
	return nil
}

func (c *WhisperClient) SendAudio(data []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.audioBuf = append(c.audioBuf, data...)
	return nil
}

func (c *WhisperClient) IsConnected() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return !c.closed
}

func (c *WhisperClient) Close() error {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil
	}
	c.closed = true
	buf := c.audioBuf
	c.mu.Unlock()

	defer func() {
		if c.onDisconnect != nil {
			c.onDisconnect()
		}
	}()

	if len(buf) == 0 {
		return nil
	}

	segments, err := c.transcribe(buf)
	if err != nil {
		if c.onError != nil {
			c.onError(err)
		}
		return err
	}
	for _, seg := range segments {
		if c.onSegment != nil {
			c.onSegment(seg)
		}
	}
	return nil
}

func (c *WhisperClient) transcribe(pcm []byte) ([]Segment, error) {
	model, err := whisper.New(c.modelPath)
	if err != nil {
		return nil, fmt.Errorf("load whisper model: %w", err)
	}
	defer model.Close()

	ctx, err := model.NewContext()
	if err != nil {
		return nil, fmt.Errorf("create whisper context: %w", err)
	}

	samples := pcm16ToFloat32(pcm)
	if err := ctx.Process(samples, nil, nil); err != nil {
		return nil, fmt.Errorf("whisper process: %w", err)
	}

	now := time.Now()
	var segs []Segment
	for {
		s, err := ctx.NextSegment()
		if err != nil {
			break
		}
		segs = append(segs, Segment{
			Text:      strings.TrimSpace(s.Text),
			Speaker:   "A",
			StartTime: s.Start.Seconds(),
			EndTime:   s.End.Seconds(),
			IsFinal:   true,
			CreatedAt: now,
		})
	}
	return segs, nil
}

// resolveWhisperModel converts a model name to an absolute path.
func resolveWhisperModel(model string) string {
	if strings.HasPrefix(model, "path:") {
		return strings.TrimPrefix(model, "path:")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".yogurt", "whisper", "ggml-"+model+".bin")
}

// pcm16ToFloat32 converts raw PCM16 LE bytes to float32 samples.
func pcm16ToFloat32(pcm []byte) []float32 {
	samples := make([]float32, len(pcm)/2)
	for i := range samples {
		s := int16(pcm[2*i]) | int16(pcm[2*i+1])<<8
		samples[i] = float32(s) / 32768.0
	}
	return samples
}
