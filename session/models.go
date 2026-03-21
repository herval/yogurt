package session

import (
	"fmt"
	"strings"
	"time"

	"github.com/herval/yogurtgo/transcription"
)

type Status int

const (
	StatusIdle      Status = iota
	StatusRecording
	StatusPaused
	StatusFinished
)

func (s Status) String() string {
	switch s {
	case StatusIdle:
		return "IDLE"
	case StatusRecording:
		return "RECORDING"
	case StatusPaused:
		return "PAUSED"
	case StatusFinished:
		return "FINISHED"
	default:
		return "UNKNOWN"
	}
}

// Session holds all data for a single recording session.
type Session struct {
	ID         string
	Name       string
	StartTime  time.Time
	EndTime    time.Time
	Status     Status
	Transcript transcription.Transcript
	FolderPath string
}

func NewSession(name string) *Session {
	return &Session{
		ID:        fmt.Sprintf("%d", time.Now().UnixNano()),
		Name:      name,
		StartTime: time.Now(),
		Status:    StatusIdle,
	}
}

func (s *Session) FolderName() string {
	ts := s.StartTime.Format("2006-01-02_15-04-05")
	if s.Name != "" {
		safe := strings.Map(func(r rune) rune {
			if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '-' || r == '_' {
				return r
			}
			return '_'
		}, s.Name)
		return fmt.Sprintf("%s_%s", ts, safe)
	}
	return ts
}

func (s *Session) Duration() time.Duration {
	if s.Status == StatusFinished && !s.EndTime.IsZero() {
		return s.EndTime.Sub(s.StartTime)
	}
	return time.Since(s.StartTime)
}

func (s *Session) DurationFormatted() string {
	d := s.Duration()
	h := int(d.Hours())
	m := int(d.Minutes()) % 60
	sec := int(d.Seconds()) % 60
	return fmt.Sprintf("%02d:%02d:%02d", h, m, sec)
}

func (s *Session) ToMetadata() map[string]any {
	return map[string]any{
		"id":            s.ID,
		"name":          s.Name,
		"start_time":    s.StartTime.Format(time.RFC3339),
		"end_time":      s.EndTime.Format(time.RFC3339),
		"duration_secs": s.Duration().Seconds(),
		"word_count":    s.Transcript.WordCount(),
		"speaker_count": len(s.Transcript.Speakers()),
	}
}
