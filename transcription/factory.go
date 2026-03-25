package transcription

// NewSTTClient returns the appropriate STTClient for the given provider.
func NewSTTClient(provider, apiKey string, sampleRate int, model string) STTClient {
	switch provider {
	case "elevenlabs":
		return NewElevenLabsClient(apiKey, sampleRate, model)
	case "whisper":
		return NewWhisperClient(sampleRate, model)
	default: // "assemblyai"
		return NewAssemblyAIClient(apiKey, sampleRate, model)
	}
}
