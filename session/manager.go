package session

import (
	"fmt"
	"log"
	"sync"
	"time"

	"github.com/herval/yogurtgo/audio"
	"github.com/herval/yogurtgo/transcription"
)

// Manager orchestrates audio capture, transcription, and session lifecycle.
type Manager struct {
	STTProvider string
	STTAPIKey   string
	SampleRate  int
	DeviceIndex int
	SessionsDir string
	STTModel    string

	OnSegment    func(transcription.Segment)
	OnStatus     func(Status)
	OnError      func(error)
	OnAudioLevel func(float64)

	mu         sync.Mutex
	current    *Session
	capture    *audio.Capture
	client     transcription.STTClient
	audioCh    chan []byte
	audioBuf   []byte
	stopStream chan struct{}
	Storage    *Storage
}

func NewManager(sttProvider, sttAPIKey string, sampleRate, deviceIndex int, sessionsDir, sttModel string) *Manager {
	return &Manager{
		STTProvider: sttProvider,
		STTAPIKey:   sttAPIKey,
		SampleRate:  sampleRate,
		DeviceIndex: deviceIndex,
		SessionsDir: sessionsDir,
		STTModel:    sttModel,
		Storage:     NewStorage(sessionsDir),
	}
}

func (m *Manager) CurrentSession() *Session {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.current
}

func (m *Manager) CurrentStatus() Status {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.current == nil {
		return StatusIdle
	}
	return m.current.Status
}

// StartSession begins a new recording and transcription session.
func (m *Manager) StartSession(name string) error {
	m.mu.Lock()
	if m.current != nil && m.current.Status == StatusRecording {
		m.mu.Unlock()
		return fmt.Errorf("already recording")
	}

	sess := NewSession(name)
	sess.Status = StatusRecording
	m.current = sess
	m.audioBuf = nil
	m.mu.Unlock()

	log.Printf("starting session: device=%d sampleRate=%d stt=%s/%s",
		m.DeviceIndex, m.SampleRate, m.STTProvider, m.STTModel)

	client := transcription.NewSTTClient(m.STTProvider, m.STTAPIKey, m.SampleRate, m.STTModel)
	client.SetOnSegment(func(seg transcription.Segment) {
		log.Printf("segment: final=%v speaker=%s text=%q", seg.IsFinal, seg.Speaker, seg.Text)
		m.mu.Lock()
		if m.current != nil {
			m.current.Transcript.AddSegment(seg)
		}
		m.mu.Unlock()
		if m.OnSegment != nil {
			m.OnSegment(seg)
		}
	})
	client.SetOnError(func(err error) {
		log.Printf("transcription error: %v", err)
		if m.OnError != nil {
			m.OnError(err)
		}
	})
	client.SetOnConnected(func() {
		log.Printf("connected to STT provider (%s)", m.STTProvider)
	})
	client.SetOnDisconnect(func() {
		log.Printf("disconnected from STT provider (%s)", m.STTProvider)
	})

	if err := client.Connect(); err != nil {
		m.mu.Lock()
		m.current = nil
		m.mu.Unlock()
		return fmt.Errorf("connect to STT (%s): %w", m.STTProvider, err)
	}

	m.mu.Lock()
	m.client = client
	m.mu.Unlock()

	// Start audio capture
	audioCh := make(chan []byte, 512)
	cap := &audio.Capture{}

	if err := cap.Start(m.DeviceIndex, m.SampleRate, audioCh); err != nil {
		_ = client.Close()
		m.mu.Lock()
		m.current = nil
		m.client = nil
		m.mu.Unlock()
		return fmt.Errorf("start audio: %w", err)
	}

	stopStream := make(chan struct{})

	m.mu.Lock()
	m.capture = cap
	m.audioCh = audioCh
	m.stopStream = stopStream
	m.mu.Unlock()

	go m.streamAudio(audioCh, client, stopStream)

	m.notifyStatus(StatusRecording)
	return nil
}

func (m *Manager) streamAudio(ch <-chan []byte, client transcription.STTClient, stop <-chan struct{}) {
	// AssemblyAI v3 requires 50–1000ms chunks. At 16kHz PCM16:
	//   50ms  = 1600 bytes
	//   100ms = 3200 bytes  ← target send size
	const minSendBytes = 1600    // 50ms minimum
	const targetSendBytes = 3200 // 100ms target

	_ = minSendBytes // used as documentation

	var sendBuf []byte

	var totalChunks int
	var silentChunks int
	flush := func() {
		if len(sendBuf) == 0 {
			return
		}
		totalChunks++
		if totalChunks <= 5 || totalChunks%50 == 0 {
			log.Printf("sending audio chunk #%d: %d bytes", totalChunks, len(sendBuf))
		}
		if err := client.SendAudio(sendBuf); err != nil {
			log.Printf("send audio error: %v", err)
		}
		sendBuf = nil
	}

	for {
		select {
		case <-stop:
			flush()
			return
		case data, ok := <-ch:
			if !ok {
				flush()
				return
			}
			m.mu.Lock()
			status := StatusIdle
			if m.current != nil {
				status = m.current.Status
			}
			m.mu.Unlock()

			if status == StatusPaused {
				sendBuf = nil
				if m.OnAudioLevel != nil {
					m.OnAudioLevel(0)
				}
				continue
			}
			if status != StatusRecording {
				flush()
				return
			}

			m.mu.Lock()
			m.audioBuf = append(m.audioBuf, data...)
			m.mu.Unlock()

			level := calcLevel(data)
			if level == 0 {
				silentChunks++
				if silentChunks == 50 {
					log.Printf("WARNING: mic appears silent (50 consecutive zero-level chunks) — check mic permissions and device selection")
				}
			} else {
				silentChunks = 0
			}
			if m.OnAudioLevel != nil {
				m.OnAudioLevel(level)
			}

			sendBuf = append(sendBuf, data...)
			if len(sendBuf) >= targetSendBytes {
				flush()
			}
		}
	}
}

func calcLevel(data []byte) float64 {
	if len(data) < 2 {
		return 0
	}
	var maxVal int16
	for i := 0; i+1 < len(data); i += 2 {
		s := int16(data[i]) | int16(data[i+1])<<8
		if s < 0 {
			s = -s
		}
		if s > maxVal {
			maxVal = s
		}
	}
	v := float64(maxVal) / 32768.0 * 2.0
	if v > 1.0 {
		v = 1.0
	}
	return v
}

// Pause pauses audio delivery to the transcriber.
func (m *Manager) Pause() {
	m.mu.Lock()
	if m.current == nil || m.current.Status != StatusRecording {
		m.mu.Unlock()
		return
	}
	m.current.Status = StatusPaused
	if m.capture != nil {
		m.capture.Pause()
	}
	m.mu.Unlock()
	m.notifyStatus(StatusPaused)
}

// Resume resumes after pause.
func (m *Manager) Resume() {
	m.mu.Lock()
	if m.current == nil || m.current.Status != StatusPaused {
		m.mu.Unlock()
		return
	}
	m.current.Status = StatusRecording
	if m.capture != nil {
		m.capture.Resume()
	}
	m.mu.Unlock()
	m.notifyStatus(StatusRecording)
}

// Finish ends the session and saves all data. Returns the folder path.
func (m *Manager) Finish() (string, error) {
	m.mu.Lock()
	sess := m.current
	if sess == nil {
		m.mu.Unlock()
		return "", nil
	}
	sess.Status = StatusFinished
	sess.EndTime = time.Now()
	stop := m.stopStream
	cap := m.capture
	client := m.client
	buf := m.audioBuf
	m.current = nil
	m.capture = nil
	m.client = nil
	m.audioBuf = nil
	m.stopStream = nil
	m.mu.Unlock()

	m.notifyStatus(StatusFinished)

	if stop != nil {
		close(stop)
	}
	if cap != nil {
		cap.Stop()
	}
	if client != nil {
		_ = client.Close()
	}

	var folder string
	if len(buf) > 0 || len(sess.Transcript.Segments) > 0 {
		var err error
		folder, err = m.Storage.Save(sess, buf)
		if err != nil {
			return "", err
		}
	}
	return folder, nil
}

// TranscribeFile processes an audio file through the STT pipeline and saves a session.
// It is a headless alternative to StartSession+Finish for batch processing.
// onProgress is called periodically with bytes sent so far (may be nil).
func (m *Manager) TranscribeFile(name, filePath string, onProgress func(sent, total int)) (string, error) {
	pcm, sampleRate, err := audio.ReadAudioFile(filePath)
	if err != nil {
		return "", fmt.Errorf("read audio file: %w", err)
	}

	// Use the file's sample rate unless the manager was explicitly configured otherwise.
	effectiveSampleRate := m.SampleRate
	if effectiveSampleRate == 0 {
		effectiveSampleRate = sampleRate
	}

	sess := NewSession(name)
	sess.Status = StatusRecording

	m.mu.Lock()
	m.current = sess
	m.audioBuf = pcm
	m.mu.Unlock()

	client := transcription.NewSTTClient(m.STTProvider, m.STTAPIKey, effectiveSampleRate, m.STTModel)
	client.SetOnSegment(func(seg transcription.Segment) {
		log.Printf("segment: final=%v speaker=%s text=%q", seg.IsFinal, seg.Speaker, seg.Text)
		m.mu.Lock()
		if m.current != nil {
			m.current.Transcript.AddSegment(seg)
		}
		m.mu.Unlock()
		if m.OnSegment != nil {
			m.OnSegment(seg)
		}
	})
	client.SetOnError(func(err error) {
		log.Printf("transcription error: %v", err)
		if m.OnError != nil {
			m.OnError(err)
		}
	})
	client.SetOnConnected(func() {
		log.Printf("connected to STT provider (%s)", m.STTProvider)
	})
	client.SetOnDisconnect(func() {
		log.Printf("disconnected from STT provider (%s)", m.STTProvider)
	})

	if err := client.Connect(); err != nil {
		m.mu.Lock()
		m.current = nil
		m.audioBuf = nil
		m.mu.Unlock()
		return "", fmt.Errorf("connect to STT (%s): %w", m.STTProvider, err)
	}

	m.mu.Lock()
	m.client = client
	m.mu.Unlock()

	// Stream audio in 100ms chunks (3200 bytes at 16kHz PCM16).
	const chunkSize = 3200
	for i := 0; i < len(pcm); i += chunkSize {
		end := i + chunkSize
		if end > len(pcm) {
			end = len(pcm)
		}
		if err := client.SendAudio(pcm[i:end]); err != nil {
			return "", fmt.Errorf("send audio: %w", err)
		}
		if onProgress != nil {
			onProgress(end, len(pcm))
		}
	}

	return m.Finish()
}

// Cancel cancels the current session without saving.
func (m *Manager) Cancel() {
	m.mu.Lock()
	sess := m.current
	stop := m.stopStream
	cap := m.capture
	client := m.client
	m.current = nil
	m.capture = nil
	m.client = nil
	m.audioBuf = nil
	m.stopStream = nil
	m.mu.Unlock()

	if sess != nil {
		m.notifyStatus(StatusIdle)
	}
	if stop != nil {
		close(stop)
	}
	if cap != nil {
		cap.Stop()
	}
	if client != nil {
		_ = client.Close()
	}
}

func (m *Manager) notifyStatus(s Status) {
	if m.OnStatus != nil {
		go m.OnStatus(s)
	}
}
