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
