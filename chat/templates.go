package chat

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// Template is a pre-made chat question with a display name and the actual prompt sent to the LLM.
type Template struct {
	Name   string `json:"name"`
	Prompt string `json:"prompt"`
}

var defaultTemplates = []Template{
	{
		Name:   "TL;DR",
		Prompt: "Give me a brief summary (tl;dr) of this recording in 2-3 sentences.",
	},
	{
		Name:   "What did I miss?",
		Prompt: "What were the most important points discussed? List the key decisions, action items, or insights I should know about.",
	},
	{
		Name:   "Action items",
		Prompt: "List all action items, tasks, or commitments mentioned in this recording. Include who is responsible if mentioned.",
	},
	{
		Name:   "Key decisions",
		Prompt: "What decisions were made during this meeting or conversation? List them clearly.",
	},
	{
		Name:   "Open questions",
		Prompt: "What questions were raised but not resolved? List any unresolved topics or follow-up items.",
	},
}

// DefaultTemplates returns the built-in set of question templates.
func DefaultTemplates() []Template {
	return defaultTemplates
}

// LoadTemplates reads templates from a JSON file.
func LoadTemplates(path string) ([]Template, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var templates []Template
	if err := json.Unmarshal(data, &templates); err != nil {
		return nil, err
	}
	return templates, nil
}

// EnsureTemplatesFile loads templates from path, creating the file with defaults if it doesn't exist.
func EnsureTemplatesFile(path string) []Template {
	templates, err := LoadTemplates(path)
	if err == nil {
		return templates
	}
	defaults := DefaultTemplates()
	data, err := json.MarshalIndent(defaults, "", "  ")
	if err != nil {
		return defaults
	}
	if mkErr := os.MkdirAll(filepath.Dir(path), 0755); mkErr == nil {
		_ = os.WriteFile(path, data, 0644)
	}
	return defaults
}
