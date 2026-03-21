package session

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/herval/yogurtgo/audio"
)

// Storage handles persisting session data to disk.
type Storage struct {
	BaseDir string
}

func NewStorage(baseDir string) *Storage {
	return &Storage{BaseDir: baseDir}
}

// Save writes all session files to a new subfolder and returns the folder path.
func (s *Storage) Save(sess *Session, pcmData []byte) (string, error) {
	folder := filepath.Join(s.BaseDir, sess.FolderName())
	if err := os.MkdirAll(folder, 0755); err != nil {
		return "", fmt.Errorf("create session folder: %w", err)
	}
	sess.FolderPath = folder

	if err := audio.WriteWAV(filepath.Join(folder, "audio.wav"), pcmData, 16000); err != nil {
		return folder, fmt.Errorf("write wav: %w", err)
	}

	if err := os.WriteFile(
		filepath.Join(folder, "transcript.txt"),
		[]byte(sess.Transcript.ToPlainText()),
		0644,
	); err != nil {
		return folder, fmt.Errorf("write transcript.txt: %w", err)
	}

	transcriptJSON, err := json.MarshalIndent(sess.Transcript, "", "  ")
	if err == nil {
		_ = os.WriteFile(filepath.Join(folder, "transcript.json"), transcriptJSON, 0644)
	}

	metaJSON, err := json.MarshalIndent(sess.ToMetadata(), "", "  ")
	if err == nil {
		_ = os.WriteFile(filepath.Join(folder, "metadata.json"), metaJSON, 0644)
	}

	return folder, nil
}

// Summary is a lightweight view of a saved session for listing.
type Summary struct {
	Folder       string
	Name         string
	StartTime    string
	DurationSecs float64
	WordCount    int
	SpeakerCount int
}

// ListSessions returns saved sessions newest-first.
func (s *Storage) ListSessions() []Summary {
	entries, err := os.ReadDir(s.BaseDir)
	if err != nil {
		return nil
	}

	var out []Summary
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		if !e.IsDir() {
			continue
		}
		metaPath := filepath.Join(s.BaseDir, e.Name(), "metadata.json")
		data, err := os.ReadFile(metaPath)
		if err != nil {
			continue
		}
		var meta map[string]any
		if err := json.Unmarshal(data, &meta); err != nil {
			continue
		}
		name, _ := meta["name"].(string)
		if name == "" {
			name = e.Name()
		}
		startTime, _ := meta["start_time"].(string)
		dur, _ := meta["duration_secs"].(float64)
		words, _ := meta["word_count"].(float64)
		speakers, _ := meta["speaker_count"].(float64)

		out = append(out, Summary{
			Folder:       filepath.Join(s.BaseDir, e.Name()),
			Name:         name,
			StartTime:    startTime,
			DurationSecs: dur,
			WordCount:    int(words),
			SpeakerCount: int(speakers),
		})
	}
	return out
}

// LoadTranscript returns the plain-text transcript for a session folder.
func (s *Storage) LoadTranscript(folder string) string {
	data, err := os.ReadFile(filepath.Join(folder, "transcript.txt"))
	if err != nil {
		return "(transcript not available)"
	}
	return string(data)
}
