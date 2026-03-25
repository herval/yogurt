package main

import (
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"

	tea "github.com/charmbracelet/bubbletea"

	"github.com/herval/yogurtgo/audio"
	"github.com/herval/yogurtgo/chat"
	"github.com/herval/yogurtgo/config"
	"github.com/herval/yogurtgo/session"
	"github.com/herval/yogurtgo/ui"
)

func main() {
	listDevices := flag.Bool("list-devices", false, "List available audio input devices and exit")
	deviceFlag := flag.Int("device", -1, "Audio input device index (-1 = default)")
	sessionsDirFlag := flag.String("sessions-dir", "", "Directory to save sessions (overrides YOGURT_SESSIONS_DIR)")
	flag.Parse()

	cfg := config.FromEnv()

	if *sessionsDirFlag != "" {
		cfg.SessionsDir = *sessionsDirFlag
	}
	if *deviceFlag >= 0 {
		cfg.AudioDevice = *deviceFlag
	}

	if *listDevices {
		// Request permission first so the device list is accurate
		ensurePermission()
		devices := audio.ListDevices()
		if len(devices) == 0 {
			fmt.Println("No audio input devices found.")
			os.Exit(1)
		}
		fmt.Println("Available audio input devices:")
		for _, d := range devices {
			fmt.Printf("  [%d] %s\n", d.Index, d.Name)
		}
		os.Exit(0)
	}

	if errs := cfg.Validate(); len(errs) > 0 {
		for _, e := range errs {
			fmt.Fprintln(os.Stderr, "Error:", e)
		}
		os.Exit(1)
	}

	// Log to file so output isn't swallowed by the TUI alt-screen
	logFile, err := os.OpenFile("yogurt.log", os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err == nil {
		log.SetOutput(logFile)
		defer logFile.Close()
	}
	log.Printf("starting yogurt (device=%d, sampleRate=%d, stt=%s/%s, llm=%s/%s)",
		cfg.AudioDevice, cfg.SampleRate, cfg.STTProvider, cfg.STTModel, cfg.LLMProvider, cfg.LLMModel)

	// Request microphone permission (triggers dialog on first run)
	ensurePermission()

	devices := audio.ListDevices()

	mgr := session.NewManager(
		cfg.STTProvider,
		cfg.STTAPIKey,
		cfg.SampleRate,
		cfg.AudioDevice,
		cfg.AbsSessionsDir(),
		cfg.STTModel,
	)

	home, _ := os.UserHomeDir()
	templatesPath := filepath.Join(home, ".yogurt", "chat_templates.json")
	templates := chat.EnsureTemplatesFile(templatesPath)

	model := ui.New(mgr, devices, cfg, templates)

	p := tea.NewProgram(model, tea.WithAltScreen())
	model.WireCallbacks(p)

	if _, err := p.Run(); err != nil {
		fmt.Fprintln(os.Stderr, "Error:", err)
		os.Exit(1)
	}
}

func ensurePermission() {
	status := audio.AuthorizationStatus()
	// 3 = authorized, 0 = not determined (will show dialog)
	if status == 2 { // denied
		fmt.Fprintln(os.Stderr, "Microphone access denied.")
		fmt.Fprintln(os.Stderr, "Enable it in: System Settings → Privacy & Security → Microphone")
		os.Exit(1)
	}
	if status != 3 {
		// Will block until user responds
		if err := audio.RequestPermission(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
	}
}
