package chat

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	openai "github.com/sashabaranov/go-openai"
)

// Message is a single entry in the chat history.
type Message struct {
	Role    string // "user" or "assistant"
	Content string
}

// Client wraps an OpenAI-compatible API for chat completions.
type Client struct {
	client   *openai.Client
	model    string
	provider string
}

// New creates a chat client for the given provider/model.
// Supported providers: "openai" (default), "gemini", "anthropic".
func New(provider, apiKey, model string) *Client {
	if model == "" {
		model = "gpt-4o-mini"
	}
	cfg := openai.DefaultConfig(apiKey)
	switch provider {
	case "gemini":
		cfg.BaseURL = "https://generativelanguage.googleapis.com/v1beta/openai/"
	case "anthropic":
		cfg.BaseURL = "https://api.anthropic.com/v1/"
	}
	return &Client{
		client:   openai.NewClientWithConfig(cfg),
		model:    model,
		provider: provider,
	}
}

// Ask sends the conversation history with a system prompt and returns the reply.
func (c *Client) Ask(ctx context.Context, systemPrompt string, history []Message, userMessage string) (string, error) {
	msgs := []openai.ChatCompletionMessage{
		{Role: openai.ChatMessageRoleSystem, Content: systemPrompt},
	}
	for _, m := range history {
		msgs = append(msgs, openai.ChatCompletionMessage{
			Role:    m.Role,
			Content: m.Content,
		})
	}
	msgs = append(msgs, openai.ChatCompletionMessage{
		Role:    openai.ChatMessageRoleUser,
		Content: userMessage,
	})

	resp, err := c.client.CreateChatCompletion(ctx, openai.ChatCompletionRequest{
		Model:    c.model,
		Messages: msgs,
	})
	if err != nil {
		return "", fmt.Errorf("chat completion: %w", err)
	}
	if len(resp.Choices) == 0 {
		return "", fmt.Errorf("no choices returned")
	}
	return resp.Choices[0].Message.Content, nil
}

// Meta holds the generated title and summary for a session.
type Meta struct {
	Title   string `json:"title"`
	Summary string `json:"summary"`
}

// GenerateMeta produces a title and summary for a transcript using the LLM.
func (c *Client) GenerateMeta(ctx context.Context, transcript string) (Meta, error) {
	if transcript == "" {
		return Meta{}, fmt.Errorf("transcript is empty")
	}

	prompt := "You are given a meeting transcript. Return ONLY a JSON object with two fields:\n" +
		"- \"title\": a concise meeting title, max 8 words\n" +
		"- \"summary\": a 2-3 sentence summary of the key points discussed\n\n" +
		"Example: {\"title\": \"Q1 Planning\", \"summary\": \"The team discussed...\"}\n\n" +
		"Transcript:\n" + transcript

	req := openai.ChatCompletionRequest{
		Model: c.model,
		Messages: []openai.ChatCompletionMessage{
			{Role: openai.ChatMessageRoleUser, Content: prompt},
		},
	}
	// Only set JSON response format for OpenAI (not all providers support it)
	if c.provider == "openai" || c.provider == "" {
		req.ResponseFormat = &openai.ChatCompletionResponseFormat{
			Type: openai.ChatCompletionResponseFormatTypeJSONObject,
		}
	}

	resp, err := c.client.CreateChatCompletion(ctx, req)
	if err != nil {
		return Meta{}, fmt.Errorf("generate meta: %w", err)
	}
	if len(resp.Choices) == 0 {
		return Meta{}, fmt.Errorf("no choices returned")
	}

	raw := resp.Choices[0].Message.Content
	// Extract JSON in case the model wraps it in text or markdown fences
	raw = extractJSON(raw)

	var meta Meta
	if err := json.Unmarshal([]byte(raw), &meta); err != nil {
		return Meta{}, fmt.Errorf("parse response: %w", err)
	}
	return meta, nil
}

// extractJSON finds the first {...} block in s.
func extractJSON(s string) string {
	start := strings.Index(s, "{")
	end := strings.LastIndex(s, "}")
	if start >= 0 && end > start {
		return s[start : end+1]
	}
	return s
}
