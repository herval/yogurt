package transcription

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"sync"
	"time"
)

const elevenLabsSTTURL = "https://api.elevenlabs.io/v1/speech-to-text"

// ElevenLabsClient is a batch STT adapter for ElevenLabs Scribe.
// Audio is buffered locally; transcription runs when Close() is called.
type ElevenLabsClient struct {
	apiKey     string
	sampleRate int
	model      string

	mu       sync.Mutex
	audioBuf []byte
	closed   bool

	onSegment    func(Segment)
	onError      func(error)
	onConnected  func()
	onDisconnect func()
}

func NewElevenLabsClient(apiKey string, sampleRate int, model string) *ElevenLabsClient {
	if model == "" {
		model = "scribe_v1"
	}
	return &ElevenLabsClient{
		apiKey:     apiKey,
		sampleRate: sampleRate,
		model:      model,
	}
}

func (c *ElevenLabsClient) SetOnSegment(fn func(Segment))    { c.onSegment = fn }
func (c *ElevenLabsClient) SetOnError(fn func(error))         { c.onError = fn }
func (c *ElevenLabsClient) SetOnConnected(fn func())           { c.onConnected = fn }
func (c *ElevenLabsClient) SetOnDisconnect(fn func())          { c.onDisconnect = fn }

func (c *ElevenLabsClient) Connect() error {
	if c.onConnected != nil {
		go c.onConnected()
	}
	return nil
}

func (c *ElevenLabsClient) SendAudio(data []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.audioBuf = append(c.audioBuf, data...)
	return nil
}

func (c *ElevenLabsClient) IsConnected() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return !c.closed
}

// Close sends the buffered audio to ElevenLabs and fires OnSegment with the results.
func (c *ElevenLabsClient) Close() error {
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

	wavData := pcm16ToWAV(buf, c.sampleRate)
	segments, err := c.transcribe(wavData)
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

type elevenLabsResponse struct {
	Text  string `json:"text"`
	Words []struct {
		Text      string  `json:"text"`
		Type      string  `json:"type"`
		Start     float64 `json:"start"`
		End       float64 `json:"end"`
		SpeakerID string  `json:"speaker_id"`
	} `json:"words"`
}

func (c *ElevenLabsClient) transcribe(wavData []byte) ([]Segment, error) {
	var body bytes.Buffer
	w := multipart.NewWriter(&body)

	_ = w.WriteField("model_id", c.model)
	_ = w.WriteField("diarize", "true")

	part, err := w.CreateFormFile("audio", "audio.wav")
	if err != nil {
		return nil, err
	}
	if _, err := part.Write(wavData); err != nil {
		return nil, err
	}
	w.Close()

	req, err := http.NewRequest("POST", elevenLabsSTTURL, &body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("xi-api-key", c.apiKey)
	req.Header.Set("Content-Type", w.FormDataContentType())

	client := &http.Client{Timeout: 120 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("elevenlabs request: %w", err)
	}
	defer resp.Body.Close()

	data, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("elevenlabs API error %d: %s", resp.StatusCode, string(data))
	}

	var result elevenLabsResponse
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, fmt.Errorf("parse elevenlabs response: %w", err)
	}

	return elevenLabsToSegments(result), nil
}

func elevenLabsToSegments(r elevenLabsResponse) []Segment {
	if len(r.Words) == 0 {
		if r.Text != "" {
			return []Segment{{
				Text:      r.Text,
				Speaker:   "A",
				IsFinal:   true,
				CreatedAt: time.Now(),
			}}
		}
		return nil
	}

	// Group consecutive words by speaker into segments.
	type group struct {
		speaker string
		words   []string
		start   float64
		end     float64
	}

	var groups []group
	for _, w := range r.Words {
		if w.Type != "word" {
			continue
		}
		spk := speakerLabel(w.SpeakerID)
		if len(groups) == 0 || groups[len(groups)-1].speaker != spk {
			groups = append(groups, group{speaker: spk, start: w.Start})
		}
		g := &groups[len(groups)-1]
		g.words = append(g.words, w.Text)
		g.end = w.End
	}

	now := time.Now()
	segs := make([]Segment, len(groups))
	for i, g := range groups {
		segs[i] = Segment{
			Text:       joinWords(g.words),
			Speaker:    g.speaker,
			StartTime:  g.start,
			EndTime:    g.end,
			Confidence: 1.0,
			IsFinal:    true,
			CreatedAt:  now,
		}
	}
	return segs
}

// speakerLabel converts "speaker_0" → "A", "speaker_1" → "B", etc.
func speakerLabel(id string) string {
	if id == "" {
		return "A"
	}
	var n int
	fmt.Sscanf(id, "speaker_%d", &n)
	return string(rune('A' + n%26))
}

func joinWords(words []string) string {
	var b bytes.Buffer
	for i, w := range words {
		if i > 0 {
			b.WriteByte(' ')
		}
		b.WriteString(w)
	}
	return b.String()
}

// pcm16ToWAV wraps raw PCM16 LE mono data in a WAV container.
func pcm16ToWAV(pcm []byte, sampleRate int) []byte {
	const numChannels = 1
	const bitsPerSample = 16
	byteRate := uint32(sampleRate * numChannels * bitsPerSample / 8)
	blockAlign := uint16(numChannels * bitsPerSample / 8)
	dataSize := uint32(len(pcm))

	var buf bytes.Buffer
	buf.WriteString("RIFF")
	binary.Write(&buf, binary.LittleEndian, uint32(36+dataSize)) //nolint
	buf.WriteString("WAVE")
	buf.WriteString("fmt ")
	binary.Write(&buf, binary.LittleEndian, uint32(16))         //nolint
	binary.Write(&buf, binary.LittleEndian, uint16(1))          // PCM
	binary.Write(&buf, binary.LittleEndian, uint16(numChannels)) //nolint
	binary.Write(&buf, binary.LittleEndian, uint32(sampleRate)) //nolint
	binary.Write(&buf, binary.LittleEndian, byteRate)           //nolint
	binary.Write(&buf, binary.LittleEndian, blockAlign)         //nolint
	binary.Write(&buf, binary.LittleEndian, uint16(bitsPerSample)) //nolint
	buf.WriteString("data")
	binary.Write(&buf, binary.LittleEndian, dataSize) //nolint
	buf.Write(pcm)
	return buf.Bytes()
}
