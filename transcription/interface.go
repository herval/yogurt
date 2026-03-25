package transcription

// STTClient is the interface implemented by all speech-to-text backends.
//
// Streaming backends (AssemblyAI) fire OnSegment callbacks during the session.
// Batch backends (ElevenLabs, Whisper) buffer audio and fire OnSegment after Close().
type STTClient interface {
	SetOnSegment(func(Segment))
	SetOnError(func(error))
	SetOnConnected(func())
	SetOnDisconnect(func())

	Connect() error
	SendAudio([]byte) error
	Close() error
	IsConnected() bool
}
