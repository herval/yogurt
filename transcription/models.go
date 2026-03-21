package transcription

import (
	"fmt"
	"time"
)

// Segment is a single piece of transcribed speech.
type Segment struct {
	Text      string    `json:"text"`
	Speaker   string    `json:"speaker"`   // "A", "B", ... or "" if unknown
	StartTime float64   `json:"start_time"` // seconds
	EndTime   float64   `json:"end_time"`
	Confidence float64  `json:"confidence"`
	IsFinal   bool      `json:"is_final"`
	CreatedAt time.Time `json:"created_at"`
}

func (s *Segment) FormatTimestamp() string {
	total := int(s.StartTime)
	h := total / 3600
	m := (total % 3600) / 60
	sec := total % 60
	return fmt.Sprintf("%02d:%02d:%02d", h, m, sec)
}

// Transcript holds all segments for a session.
type Transcript struct {
	Segments []Segment `json:"segments"`
	partial  *Segment
}

func (t *Transcript) AddSegment(seg Segment) {
	if seg.IsFinal {
		t.Segments = append(t.Segments, seg)
		t.partial = nil
	} else {
		t.partial = &seg
	}
}

func (t *Transcript) Partial() *Segment {
	return t.partial
}

func (t *Transcript) WordCount() int {
	count := 0
	for _, s := range t.Segments {
		for _, r := range s.Text {
			if r == ' ' {
				count++
			}
		}
		if len(s.Text) > 0 {
			count++ // count last word
		}
	}
	return count
}

func (t *Transcript) Speakers() []string {
	seen := map[string]bool{}
	var out []string
	for _, s := range t.Segments {
		if s.Speaker != "" && !seen[s.Speaker] {
			seen[s.Speaker] = true
			out = append(out, s.Speaker)
		}
	}
	return out
}

func (t *Transcript) ToPlainText() string {
	out := ""
	for _, s := range t.Segments {
		speaker := "Unknown"
		if s.Speaker != "" {
			speaker = "Speaker " + s.Speaker
		}
		out += fmt.Sprintf("[%s] %s:\n%s\n\n", s.FormatTimestamp(), speaker, s.Text)
	}
	return out
}
