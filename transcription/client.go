package transcription

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

const streamingURL = "wss://streaming.assemblyai.com/v3/ws"

// Client streams PCM16 audio to AssemblyAI and delivers transcript segments.
type Client struct {
	apiKey      string
	sampleRate  int
	speechModel string

	OnSegment    func(Segment)
	OnError      func(error)
	OnConnected  func()
	OnDisconnect func()

	mu          sync.Mutex
	conn        *websocket.Conn
	connected   bool
	turnOrder   int
	done        chan struct{}
}

func NewAssemblyAIClient(apiKey string, sampleRate int, speechModel string) *Client {
	return &Client{
		apiKey:      apiKey,
		sampleRate:  sampleRate,
		speechModel: speechModel,
		done:        make(chan struct{}),
	}
}

func (c *Client) SetOnSegment(fn func(Segment))    { c.OnSegment = fn }
func (c *Client) SetOnError(fn func(error))         { c.OnError = fn }
func (c *Client) SetOnConnected(fn func())           { c.OnConnected = fn }
func (c *Client) SetOnDisconnect(fn func())          { c.OnDisconnect = fn }

func (c *Client) Connect() error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if c.connected {
		return nil
	}

	u, _ := url.Parse(streamingURL)
	q := u.Query()
	q.Set("sample_rate", fmt.Sprintf("%d", c.sampleRate))
	q.Set("encoding", "pcm_s16le")
	q.Set("speech_model", c.speechModel)
	q.Set("diarization", "true")
	u.RawQuery = q.Encode()

	hdr := http.Header{}
	hdr.Set("Authorization", c.apiKey)

	dialer := websocket.Dialer{
		HandshakeTimeout: 15 * time.Second,
	}
	conn, _, err := dialer.Dial(u.String(), hdr)
	if err != nil {
		return fmt.Errorf("websocket connect: %w", err)
	}

	c.conn = conn
	c.connected = true
	c.done = make(chan struct{})

	if c.OnConnected != nil {
		go c.OnConnected()
	}

	go c.receiveLoop()
	return nil
}

func (c *Client) receiveLoop() {
	defer func() {
		c.mu.Lock()
		c.connected = false
		c.mu.Unlock()
		if c.OnDisconnect != nil {
			c.OnDisconnect()
		}
		close(c.done)
	}()

	for {
		_, msg, err := c.conn.ReadMessage()
		if err != nil {
			if !websocket.IsCloseError(err, websocket.CloseNormalClosure, websocket.CloseGoingAway) {
				log.Printf("websocket read error: %v", err)
				if c.OnError != nil {
					c.OnError(err)
				}
			}
			return
		}

		var data map[string]any
		if err := json.Unmarshal(msg, &data); err != nil {
			log.Printf("json parse error: %v", err)
			continue
		}

		c.handleMessage(data)
	}
}

func (c *Client) handleMessage(data map[string]any) {
	msgType, _ := data["type"].(string)
	switch msgType {
	case "Begin":
		// session started, nothing to do
	case "Turn":
		c.handleTurn(data)
	case "Termination":
		// normal end
	case "Error":
		errMsg, _ := data["error"].(string)
		if c.OnError != nil {
			c.OnError(fmt.Errorf("assemblyai: %s", errMsg))
		}
	}
}

func (c *Client) handleTurn(data map[string]any) {
	text, _ := data["transcript"].(string)
	if text == "" {
		return
	}

	turnOrder := 0
	if v, ok := data["turn_order"].(float64); ok {
		turnOrder = int(v)
	}
	endOfTurn, _ := data["end_of_turn"].(bool)

	var startTime, endTime, confidence float64 = 0, 0, 1.0
	var speakerID string
	if words, ok := data["words"].([]any); ok && len(words) > 0 {
		if w0, ok := words[0].(map[string]any); ok {
			if v, ok := w0["start"].(float64); ok {
				startTime = v / 1000.0
			}
			if s, ok := w0["speaker"].(string); ok {
				speakerID = s
			}
		}
		if wN, ok := words[len(words)-1].(map[string]any); ok {
			if v, ok := wN["end"].(float64); ok {
				endTime = v / 1000.0
			}
		}
		var total float64
		for _, w := range words {
			if wm, ok := w.(map[string]any); ok {
				if v, ok := wm["confidence"].(float64); ok {
					total += v
				} else {
					total += 1.0
				}
			}
		}
		confidence = total / float64(len(words))
	}

	// Map "speaker_0" -> "A", "speaker_1" -> "B", etc.
	// Fall back to turn_order if no speaker field (diarization not available).
	speaker := string(rune('A' + turnOrder%26))
	if speakerID != "" {
		var n int
		fmt.Sscanf(speakerID, "speaker_%d", &n)
		speaker = string(rune('A' + n%26))
	}

	seg := Segment{
		Text:       text,
		Speaker:    speaker,
		StartTime:  startTime,
		EndTime:    endTime,
		Confidence: confidence,
		IsFinal:    endOfTurn,
		CreatedAt:  time.Now(),
	}

	c.turnOrder = turnOrder
	if c.OnSegment != nil {
		c.OnSegment(seg)
	}
}

// SendAudio sends raw PCM16 binary audio to AssemblyAI.
func (c *Client) SendAudio(data []byte) error {
	c.mu.Lock()
	conn := c.conn
	connected := c.connected
	c.mu.Unlock()

	if !connected || conn == nil {
		return nil
	}
	return conn.WriteMessage(websocket.BinaryMessage, data)
}

// Close gracefully terminates the session.
func (c *Client) Close() error {
	c.mu.Lock()
	conn := c.conn
	connected := c.connected
	c.mu.Unlock()

	if !connected || conn == nil {
		return nil
	}

	terminate, _ := json.Marshal(map[string]string{"type": "Terminate"})
	_ = conn.WriteMessage(websocket.TextMessage, terminate)
	time.Sleep(300 * time.Millisecond)

	err := conn.WriteMessage(websocket.CloseMessage,
		websocket.FormatCloseMessage(websocket.CloseNormalClosure, ""))
	conn.Close()

	// Wait for receiveLoop to finish
	select {
	case <-c.done:
	case <-time.After(2 * time.Second):
	}

	return err
}

func (c *Client) IsConnected() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.connected
}
