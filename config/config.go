package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/joho/godotenv"
)

type Config struct {
	// STT
	STTProvider string // "assemblyai", "elevenlabs", "whisper"
	STTModel    string // model name within the provider
	STTAPIKey   string // API key for the STT provider

	// LLM (optional — chat panel won't work without it)
	LLMProvider string // "openai", "gemini", "anthropic"
	LLMModel    string // model name within the provider
	LLMAPIKey   string // API key for the LLM provider

	// Audio / misc
	SessionsDir string
	SampleRate  int
	AudioDevice int // -1 = default
	DeviceName  string
}

func FromEnv() *Config {
	_ = godotenv.Load()

	cfg := &Config{
		SessionsDir: envOrDefault("YOGURT_SESSIONS_DIR", "./sessions"),
		SampleRate:  16000,
		AudioDevice: -1,
	}

	// --- STT ---
	// STT_MODEL=assemblyai/universal-streaming-multilingual
	// Falls back to old YOGURT_SPEECH_MODEL + assemblyai provider.
	sttRaw := os.Getenv("STT_MODEL")
	if sttRaw == "" {
		oldModel := envOrDefault("YOGURT_SPEECH_MODEL", "universal-streaming-multilingual")
		sttRaw = "assemblyai/" + oldModel
	}
	cfg.STTProvider, cfg.STTModel = parseProviderModel(sttRaw, "assemblyai")
	cfg.STTAPIKey = providerAPIKey(cfg.STTProvider)

	// --- LLM ---
	// LLM_MODEL=openai/gpt-4o-mini
	// Falls back to old OPENAI_MODEL + openai provider.
	llmRaw := os.Getenv("LLM_MODEL")
	if llmRaw == "" {
		oldModel := envOrDefault("OPENAI_MODEL", "gpt-4o-mini")
		llmRaw = "openai/" + oldModel
	}
	cfg.LLMProvider, cfg.LLMModel = parseProviderModel(llmRaw, "openai")
	cfg.LLMAPIKey = providerAPIKey(cfg.LLMProvider)

	// --- Audio ---
	if sr := os.Getenv("YOGURT_SAMPLE_RATE"); sr != "" {
		if v, err := strconv.Atoi(sr); err == nil {
			cfg.SampleRate = v
		}
	}
	if dev := os.Getenv("YOGURT_AUDIO_DEVICE"); dev != "" {
		if v, err := strconv.Atoi(dev); err == nil {
			cfg.AudioDevice = v
		} else {
			cfg.DeviceName = dev
		}
	}

	return cfg
}

// parseProviderModel splits "provider/model" → (provider, model).
// If there is no "/" the defaultProvider is used.
func parseProviderModel(s, defaultProvider string) (provider, model string) {
	if idx := strings.Index(s, "/"); idx >= 0 {
		return strings.ToLower(s[:idx]), s[idx+1:]
	}
	return defaultProvider, s
}

// providerAPIKey looks up {PROVIDER}_API_KEY from the environment.
func providerAPIKey(provider string) string {
	return os.Getenv(strings.ToUpper(provider) + "_API_KEY")
}

func (c *Config) Validate() []string {
	var errs []string
	if c.STTAPIKey == "" && c.STTProvider != "whisper" {
		key := strings.ToUpper(c.STTProvider) + "_API_KEY"
		errs = append(errs, fmt.Sprintf("%s is not set (required for STT provider %q)", key, c.STTProvider))
	}
	valid := map[int]bool{8000: true, 16000: true, 22050: true, 44100: true, 48000: true}
	if !valid[c.SampleRate] {
		errs = append(errs, fmt.Sprintf("invalid sample rate: %d", c.SampleRate))
	}
	return errs
}

func (c *Config) AbsSessionsDir() string {
	abs, err := filepath.Abs(c.SessionsDir)
	if err != nil {
		return c.SessionsDir
	}
	return abs
}

func envOrDefault(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}
