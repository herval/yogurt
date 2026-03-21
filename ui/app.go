package ui

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"

	"github.com/herval/yogurtgo/audio"
	"github.com/herval/yogurtgo/chat"
	"github.com/herval/yogurtgo/config"
	"github.com/herval/yogurtgo/session"
	"github.com/herval/yogurtgo/transcription"
)

// ---- Message types ----

type transcriptMsg struct{ seg transcription.Segment }
type statusMsg struct{ s session.Status }
type audioLevelMsg struct{ level float64 }
type errorMsg struct{ err error }
type tickMsg struct{}
type saveResultMsg struct {
	folder string
	err    error
}
type chatResponseMsg struct {
	content string
	err     error
}

// ---- Styles ----

var (
	titleStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("12"))
	recordStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("9")).Bold(true)
	pauseStyle   = lipgloss.NewStyle().Foreground(lipgloss.Color("11")).Bold(true)
	idleStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("7"))
	speakerStyle = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("10"))
	timeStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("14"))
	dimStyle     = lipgloss.NewStyle().Faint(true)
	errorStyle   = lipgloss.NewStyle().Foreground(lipgloss.Color("9"))
	noticeStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("11"))
	userStyle    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("12"))
	aiStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("13"))
)

// ---- Transcript line ----

type line struct {
	seg     transcription.Segment
	partial bool
}

// ---- Model ----

type Model struct {
	mgr     *session.Manager
	devices []audio.Device

	status     session.Status
	audioLevel float64
	duration   string
	lines      []line
	partialIdx int
	scroll     int
	width      int
	height     int
	notice     string
	noticeErr  bool

	// device selection
	selectingMic bool
	micListIdx   int

	// chat panel
	openAIKey   string
	chatModel   string
	chatOpen    bool
	chatInput   textinput.Model
	chatMsgs    []chat.Message
	chatScroll  int
	chatLoading bool
}

func New(mgr *session.Manager, devices []audio.Device, cfg *config.Config) *Model {
	ti := textinput.New()
	ti.Placeholder = "Ask about the transcript..."
	ti.CharLimit = 500

	return &Model{
		mgr:        mgr,
		devices:    devices,
		status:     session.StatusIdle,
		partialIdx: -1,
		openAIKey:  cfg.OpenAIKey,
		chatModel:  cfg.ChatModel,
		chatInput:  ti,
	}
}

func (m *Model) Init() tea.Cmd {
	return tick()
}

func tick() tea.Cmd {
	return tea.Tick(time.Second, func(t time.Time) tea.Msg {
		return tickMsg{}
	})
}

func (m *Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	// Always forward messages to the textinput when chat is open
	// (needed for cursor blink etc.), but don't let it consume key events yet.
	if m.chatOpen {
		if _, isKey := msg.(tea.KeyMsg); !isKey {
			var tiCmd tea.Cmd
			m.chatInput, tiCmd = m.chatInput.Update(msg)
			if tiCmd != nil {
				// handle non-key updates and continue processing below
				_ = tiCmd
			}
		}
	}

	switch msg := msg.(type) {

	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height

	case tickMsg:
		if sess := m.mgr.CurrentSession(); sess != nil {
			m.duration = sess.DurationFormatted()
		}
		return m, tick()

	case transcriptMsg:
		seg := msg.seg
		if seg.IsFinal {
			if m.partialIdx >= 0 {
				m.lines[m.partialIdx] = line{seg: seg, partial: false}
			} else {
				m.lines = append(m.lines, line{seg: seg})
			}
			m.partialIdx = -1
		} else {
			if m.partialIdx >= 0 {
				m.lines[m.partialIdx] = line{seg: seg, partial: true}
			} else {
				m.lines = append(m.lines, line{seg: seg, partial: true})
				m.partialIdx = len(m.lines) - 1
			}
		}

	case statusMsg:
		m.status = msg.s
		if msg.s == session.StatusIdle || msg.s == session.StatusFinished {
			m.duration = "00:00:00"
			m.audioLevel = 0
		}

	case audioLevelMsg:
		m.audioLevel = msg.level

	case errorMsg:
		m.notice = msg.err.Error()
		m.noticeErr = true

	case saveResultMsg:
		if msg.err != nil {
			m.notice = "Error saving: " + msg.err.Error()
			m.noticeErr = true
		} else if msg.folder != "" {
			m.notice = "Saved to " + msg.folder
			m.noticeErr = false
		} else {
			m.notice = "Session ended (nothing to save)"
			m.noticeErr = false
		}

	case chatResponseMsg:
		m.chatLoading = false
		content := msg.content
		if msg.err != nil {
			content = "Error: " + msg.err.Error()
		}
		m.chatMsgs = append(m.chatMsgs, chat.Message{Role: "assistant", Content: content})

	case tea.KeyMsg:
		if m.chatOpen {
			return m.handleChatKey(msg)
		}
		return m.handleKey(msg)
	}

	return m, nil
}

func (m *Model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	if m.selectingMic {
		switch msg.String() {
		case "up", "k":
			if m.micListIdx > 0 {
				m.micListIdx--
			}
		case "down", "j":
			if m.micListIdx < len(m.devices)-1 {
				m.micListIdx++
			}
		case "enter":
			if m.micListIdx < len(m.devices) {
				dev := m.devices[m.micListIdx]
				m.mgr.DeviceIndex = dev.Index
				m.notice = "Microphone: " + dev.Name
				m.noticeErr = false
			}
			m.selectingMic = false
		case "esc", "q":
			m.selectingMic = false
		}
		return m, nil
	}

	switch msg.String() {
	case "n", "N":
		if m.status == session.StatusIdle || m.status == session.StatusFinished {
			m.lines = nil
			m.partialIdx = -1
			m.duration = "00:00:00"
			m.notice = ""
			return m, m.cmdStartSession()
		}
	case "p", "P":
		switch m.status {
		case session.StatusRecording:
			m.mgr.Pause()
		case session.StatusPaused:
			m.mgr.Resume()
		}
	case "f", "F":
		if m.status == session.StatusRecording || m.status == session.StatusPaused {
			return m, m.cmdFinish()
		}
	case "m", "M":
		if m.status != session.StatusRecording {
			m.selectingMic = true
			m.micListIdx = 0
		}
	case "c", "C":
		if m.openAIKey == "" {
			m.notice = "Set OPENAI_API_KEY to enable chat"
			m.noticeErr = true
			return m, nil
		}
		m.chatOpen = true
		m.chatScroll = 0
		m.chatInput.Focus()
		return m, textinput.Blink
	case "q", "Q", "ctrl+c":
		if m.status == session.StatusRecording || m.status == session.StatusPaused {
			return m, tea.Sequence(m.cmdFinish(), tea.Quit)
		}
		return m, tea.Quit
	case "up":
		m.scroll++
	case "down":
		if m.scroll > 0 {
			m.scroll--
		}
	}
	return m, nil
}

func (m *Model) handleChatKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "esc", "ctrl+c":
		m.chatOpen = false
		m.chatInput.Blur()
		return m, nil
	case "enter":
		text := strings.TrimSpace(m.chatInput.Value())
		if text == "" || m.chatLoading {
			return m, nil
		}
		m.chatMsgs = append(m.chatMsgs, chat.Message{Role: "user", Content: text})
		m.chatInput.Reset()
		m.chatLoading = true
		m.chatScroll = 0
		return m, m.cmdAsk(text)
	case "up":
		m.chatScroll++
		return m, nil
	case "down":
		if m.chatScroll > 0 {
			m.chatScroll--
		}
		return m, nil
	default:
		var tiCmd tea.Cmd
		m.chatInput, tiCmd = m.chatInput.Update(msg)
		return m, tiCmd
	}
}

func (m *Model) cmdStartSession() tea.Cmd {
	return func() tea.Msg {
		if err := m.mgr.StartSession(""); err != nil {
			return errorMsg{err}
		}
		return nil
	}
}

func (m *Model) cmdFinish() tea.Cmd {
	return func() tea.Msg {
		folder, err := m.mgr.Finish()
		return saveResultMsg{folder: folder, err: err}
	}
}

func (m *Model) cmdAsk(userMsg string) tea.Cmd {
	// Snapshot on UI goroutine before launching background work
	transcript := ""
	if sess := m.mgr.CurrentSession(); sess != nil {
		transcript = sess.Transcript.ToPlainText()
	}
	// History excludes the message we just appended (it's the user turn)
	history := make([]chat.Message, len(m.chatMsgs)-1)
	copy(history, m.chatMsgs[:len(m.chatMsgs)-1])

	systemPrompt := "You are a helpful assistant answering questions about a live meeting recording. " +
		"Answer concisely based on the transcript below. If the transcript is empty or the answer isn't there, say so.\n\n" +
		"Transcript so far:\n" + transcript

	client := chat.New(m.openAIKey, m.chatModel)

	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		reply, err := client.Ask(ctx, systemPrompt, history, userMsg)
		return chatResponseMsg{content: reply, err: err}
	}
}

// ---- View ----

func (m *Model) View() string {
	if m.width == 0 {
		return "Loading..."
	}

	if m.selectingMic {
		return m.viewMicSelect()
	}

	var b strings.Builder
	b.WriteString(m.renderHeader())
	b.WriteByte('\n')

	if m.chatOpen {
		transcriptW := (m.width * 60) / 100
		chatW := m.width - transcriptW
		left := m.renderTranscriptPane(transcriptW)
		right := m.renderChatPane(chatW)
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, left, right))
	} else {
		b.WriteString(m.renderTranscriptPane(m.width))
	}

	b.WriteByte('\n')
	b.WriteString(m.renderStatusBar())
	b.WriteByte('\n')
	b.WriteString(m.renderControls())

	if m.notice != "" {
		b.WriteByte('\n')
		if m.noticeErr {
			b.WriteString(errorStyle.Render("  " + m.notice))
		} else {
			b.WriteString(noticeStyle.Render("  " + m.notice))
		}
	}

	return b.String()
}

func (m *Model) renderHeader() string {
	title := titleStyle.Render("YOGURT - Meeting Recorder")

	var indicator string
	switch m.status {
	case session.StatusRecording:
		indicator = recordStyle.Render("● RECORDING")
	case session.StatusPaused:
		indicator = pauseStyle.Render("⏸ PAUSED")
	case session.StatusFinished:
		indicator = idleStyle.Render("○ FINISHED")
	default:
		indicator = idleStyle.Render("○ IDLE")
	}

	gap := m.width - lipgloss.Width(title) - lipgloss.Width(indicator) - 2
	if gap < 1 {
		gap = 1
	}
	return " " + title + strings.Repeat(" ", gap) + indicator
}

func (m *Model) paneHeight() int {
	// rows used: header(1) + newline(1) + statusbar(1) + newline(1) + controls(1) + notice(1) + some buffer
	available := m.height - 7
	if available < 3 {
		available = 3
	}
	return available
}

func (m *Model) renderTranscriptPane(width int) string {
	available := m.paneHeight()
	innerWidth := width - 4
	if innerWidth < 10 {
		innerWidth = 10
	}

	displayLines := m.buildDisplayLines(innerWidth)
	total := len(displayLines)

	end := total - m.scroll
	if end < 0 {
		end = 0
	}
	start := end - available
	if start < 0 {
		start = 0
	}
	visible := displayLines[start:end]

	var b strings.Builder
	border := strings.Repeat("─", width-2)
	blank := strings.Repeat(" ", width-4)

	b.WriteString("┌" + border + "┐\n")
	for i := 0; i < available; i++ {
		if i < len(visible) {
			b.WriteString("│ " + truncatePad(visible[i], width-4) + " │\n")
		} else {
			b.WriteString("│ " + blank + " │\n")
		}
	}
	b.WriteString("└" + border + "┘")
	return b.String()
}

func (m *Model) renderChatPane(width int) string {
	available := m.paneHeight()
	innerWidth := width - 4
	if innerWidth < 10 {
		innerWidth = 10
	}
	// Reserve 3 rows for the input area (divider + input + bottom border)
	msgHeight := available - 3
	if msgHeight < 1 {
		msgHeight = 1
	}

	// Build message lines
	var allLines []string
	for _, msg := range m.chatMsgs {
		var label string
		var style lipgloss.Style
		if msg.Role == "user" {
			label = "You"
			style = userStyle
		} else {
			label = "AI"
			style = aiStyle
		}
		allLines = append(allLines, style.Render(label+":"))
		for _, l := range wordWrap(msg.Content, innerWidth-2) {
			allLines = append(allLines, "  "+l)
		}
		allLines = append(allLines, "")
	}
	if m.chatLoading {
		allLines = append(allLines, dimStyle.Render("  thinking..."))
	}
	if len(allLines) == 0 {
		allLines = append(allLines, dimStyle.Render("  Ask anything about the recording..."))
	}

	// Scroll windowing
	total := len(allLines)
	end := total - m.chatScroll
	if end < 0 {
		end = 0
	}
	start := end - msgHeight
	if start < 0 {
		start = 0
	}
	visible := allLines[start:end]

	blank := strings.Repeat(" ", innerWidth)
	border := strings.Repeat("─", width-2)

	var b strings.Builder
	b.WriteString("┌" + border + "┐\n")
	for i := 0; i < msgHeight; i++ {
		if i < len(visible) {
			b.WriteString("│ " + truncatePad(visible[i], innerWidth) + " │\n")
		} else {
			b.WriteString("│ " + blank + " │\n")
		}
	}
	// Input area
	b.WriteString("├" + border + "┤\n")
	inputView := m.chatInput.View()
	b.WriteString("│ " + truncatePad(inputView, innerWidth) + " │\n")
	b.WriteString("│ " + truncatePad(dimStyle.Render("Enter to send • Esc to close"), innerWidth) + " │\n")
	b.WriteString("└" + border + "┘")
	return b.String()
}

func (m *Model) renderStatusBar() string {
	meter := m.levelMeter()
	sess := m.mgr.CurrentSession()
	words := 0
	speakers := 0
	if sess != nil {
		words = sess.Transcript.WordCount()
		speakers = len(sess.Transcript.Speakers())
	}
	s := fmt.Sprintf("  Duration: %s  │  Words: %d  │  Speakers: %d  │  %s",
		m.duration, words, speakers, meter)
	return lipgloss.NewStyle().
		Background(lipgloss.Color("18")).
		Foreground(lipgloss.Color("7")).
		Width(m.width).
		Render(s)
}

func (m *Model) levelMeter() string {
	bars := 8
	filled := int(m.audioLevel * float64(bars))
	var b strings.Builder
	for i := 0; i < bars; i++ {
		if i < filled {
			switch {
			case i < 5:
				b.WriteString(lipgloss.NewStyle().Foreground(lipgloss.Color("10")).Render("▄"))
			case i < 7:
				b.WriteString(lipgloss.NewStyle().Foreground(lipgloss.Color("11")).Render("▄"))
			default:
				b.WriteString(lipgloss.NewStyle().Foreground(lipgloss.Color("9")).Render("▄"))
			}
		} else {
			b.WriteString(dimStyle.Render("▁"))
		}
	}
	return b.String()
}

func (m *Model) renderControls() string {
	var parts []string
	if m.status == session.StatusIdle || m.status == session.StatusFinished {
		parts = append(parts, "[N]ew Session")
	}
	if m.status == session.StatusRecording {
		parts = append(parts, "[P]ause")
		parts = append(parts, "[F]inish")
	}
	if m.status == session.StatusPaused {
		parts = append(parts, "[P]Resume")
		parts = append(parts, "[F]Finish")
	}
	if m.status != session.StatusRecording {
		parts = append(parts, "[M]ic")
	}
	if m.chatOpen {
		parts = append(parts, "[Esc]Close Chat")
	} else {
		parts = append(parts, "[C]hat")
	}
	parts = append(parts, "[Q]uit")
	return "  " + strings.Join(parts, "  │  ")
}

func (m *Model) viewMicSelect() string {
	var b strings.Builder
	b.WriteString(titleStyle.Render("Select Microphone") + "\n\n")
	for i, dev := range m.devices {
		cursor := "  "
		if i == m.micListIdx {
			cursor = "> "
		}
		b.WriteString(cursor + dev.Name + "\n")
	}
	b.WriteString("\n" + dimStyle.Render("Enter to select • Esc to cancel"))
	return b.String()
}

// buildDisplayLines produces word-wrapped rendered strings for the transcript.
func (m *Model) buildDisplayLines(innerWidth int) []string {
	if innerWidth < 20 {
		innerWidth = 20
	}
	var out []string
	for _, l := range m.lines {
		seg := l.seg
		ts := timeStyle.Render("[" + seg.FormatTimestamp() + "]")
		sp := speakerStyle.Render("Speaker " + seg.Speaker)
		header := ts + " " + sp
		if l.partial {
			header += dimStyle.Render(" (partial)")
		}
		out = append(out, header)
		for _, wrapped := range wordWrap(seg.Text, innerWidth-2) {
			if l.partial {
				out = append(out, "  "+dimStyle.Render(wrapped))
			} else {
				out = append(out, "  "+wrapped)
			}
		}
		out = append(out, "")
	}
	return out
}

// wordWrap breaks text into lines of at most width visible characters.
func wordWrap(text string, width int) []string {
	if width <= 0 {
		return []string{text}
	}
	words := strings.Fields(text)
	if len(words) == 0 {
		return []string{""}
	}
	var lines []string
	current := ""
	for _, word := range words {
		if current == "" {
			current = word
		} else if len(current)+1+len(word) <= width {
			current += " " + word
		} else {
			lines = append(lines, current)
			current = word
		}
	}
	if current != "" {
		lines = append(lines, current)
	}
	return lines
}

// truncatePad truncates or pads s to exactly w visible characters.
func truncatePad(s string, w int) string {
	visible := lipgloss.Width(s)
	if visible > w {
		runes := []rune(s)
		if len(runes) > w {
			return string(runes[:w])
		}
	}
	if visible < w {
		return s + strings.Repeat(" ", w-visible)
	}
	return s
}

// WireCallbacks connects session manager callbacks to the Bubble Tea program.
func (m *Model) WireCallbacks(p *tea.Program) {
	m.mgr.OnSegment = func(seg transcription.Segment) {
		p.Send(transcriptMsg{seg})
	}
	m.mgr.OnStatus = func(s session.Status) {
		p.Send(statusMsg{s})
	}
	m.mgr.OnError = func(err error) {
		p.Send(errorMsg{err})
	}
	m.mgr.OnAudioLevel = func(level float64) {
		p.Send(audioLevelMsg{level})
	}
}
