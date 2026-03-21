package chat

import (
	"context"
	"encoding/json"
	"fmt"

	openai "github.com/sashabaranov/go-openai"
)

// Message is a single entry in the chat history.
type Message struct {
	Role    string // "user" or "assistant"
	Content string
}

// Client wraps the OpenAI API for chat completions.
type Client struct {
	client *openai.Client
	model  string
}

func New(apiKey, model string) *Client {
	if model == "" {
		model = openai.GPT4oMini
	}
	return &Client{
		client: openai.NewClient(apiKey),
		model:  model,
	}
}

// Ask sends the conversation history with a system prompt and returns the reply.
// The transcript is injected into the system prompt on every call so the model
// always sees the latest text.
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

	prompt := "You are given a meeting transcript. Return a JSON object with two fields:\n" +
		"- \"title\": a concise meeting title, max 8 words\n" +
		"- \"summary\": a 2-3 sentence summary of the key points discussed\n\n" +
		"Transcript:\n" + transcript

	resp, err := c.client.CreateChatCompletion(ctx, openai.ChatCompletionRequest{
		Model: c.model,
		Messages: []openai.ChatCompletionMessage{
			{Role: openai.ChatMessageRoleUser, Content: prompt},
		},
		ResponseFormat: &openai.ChatCompletionResponseFormat{
			Type: openai.ChatCompletionResponseFormatTypeJSONObject,
		},
	})
	if err != nil {
		return Meta{}, fmt.Errorf("generate meta: %w", err)
	}
	if len(resp.Choices) == 0 {
		return Meta{}, fmt.Errorf("no choices returned")
	}

	var meta Meta
	if err := json.Unmarshal([]byte(resp.Choices[0].Message.Content), &meta); err != nil {
		return Meta{}, fmt.Errorf("parse response: %w", err)
	}
	return meta, nil
}
