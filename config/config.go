package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"

	"github.com/joho/godotenv"
)

type Config struct {
	APIKey      string
	SessionsDir string
	SampleRate  int
	AudioDevice int // -1 = default
	DeviceName  string
	SpeechModel string // e.g. "universal-streaming-english", "universal-streaming"
}

func FromEnv() *Config {
	// Load .env if present (ignore error if not found)
	_ = godotenv.Load()

	cfg := &Config{
		APIKey:      os.Getenv("ASSEMBLYAI_API_KEY"),
		SessionsDir: envOrDefault("YOGURT_SESSIONS_DIR", "./sessions"),
		SampleRate:  16000,
		AudioDevice: -1,
		SpeechModel: envOrDefault("YOGURT_SPEECH_MODEL", "universal-streaming-multilingual"),
	}

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

func (c *Config) Validate() []string {
	var errs []string
	if c.APIKey == "" {
		errs = append(errs, "ASSEMBLYAI_API_KEY is not set — get your key at https://www.assemblyai.com/")
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
