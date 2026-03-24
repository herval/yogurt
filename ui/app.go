package ui

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/glamour"
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
	folder     string
	transcript string
	err        error
}
type chatResponseMsg struct {
	content string
	err     error
}
type metaGeneratedMsg struct {
	folder  string
	title   string
	summary string
	err     error
}
type sessionsLoadedMsg struct{ sessions []session.Summary }

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

	// home screen (session list)
	homeMode       bool
	sessions       []session.Summary
	sessionIdx     int
	viewingSession    *session.Summary // nil when not viewing a past session
	viewLines         []string         // pre-rendered lines for a viewed session
	viewTranscriptRaw string           // plain text used as chat context

	// chat panel
	openAIKey   string
	chatModel   string
	chatOpen    bool
	chatInput   textinput.Model
	chatMsgs    []chat.Message
	chatScroll  int
	chatLoading bool

	// template picker modal
	templates    []chat.Template
	templateOpen bool
	templateIdx  int

	// delete confirmation
	confirmDelete bool

	// cached markdown renderer for chat pane (avoid per-render terminal queries)
	mdRenderer *glamour.TermRenderer
}

func New(mgr *session.Manager, devices []audio.Device, cfg *config.Config, templates []chat.Template) *Model {
	ti := textinput.New()
	ti.Placeholder = "Ask about the transcript..."
	ti.CharLimit = 500

	// Initialize the markdown renderer once, before bubbletea takes over the
	// terminal. WithAutoStyle() queries the terminal background color via OSC 11
	// and can block for up to 5s once the TUI is running.
	renderer, _ := glamour.NewTermRenderer(glamour.WithStandardStyle("dark"))

	return &Model{
		mgr:        mgr,
		devices:    devices,
		status:     session.StatusIdle,
		partialIdx: -1,
		homeMode:   true,
		openAIKey:  cfg.OpenAIKey,
		chatModel:  cfg.ChatModel,
		chatInput:  ti,
		templates:  templates,
		mdRenderer: renderer,
	}
}

func (m *Model) Init() tea.Cmd {
	return tea.Batch(tick(), m.cmdLoadSessions())
}

func (m *Model) cmdLoadSessions() tea.Cmd {
	return func() tea.Msg {
		return sessionsLoadedMsg{sessions: m.mgr.Storage.ListSessions()}
	}
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
		// Recreate the markdown renderer with the correct word-wrap width for
		// the chat pane (60% of terminal width, minus borders).
		chatInnerW := (msg.Width*60/100) - 6
		if chatInnerW < 20 {
			chatInnerW = 20
		}
		if r, err := glamour.NewTermRenderer(
			glamour.WithStandardStyle("dark"),
			glamour.WithWordWrap(chatInnerW),
		); err == nil {
			m.mdRenderer = r
		}

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
		if msg.s == session.StatusRecording {
			m.homeMode = false
		}

	case audioLevelMsg:
		m.audioLevel = msg.level

	case errorMsg:
		m.notice = msg.err.Error()
		m.noticeErr = true

	case sessionsLoadedMsg:
		m.sessions = msg.sessions
		if m.sessionIdx >= len(m.sessions) {
			m.sessionIdx = 0
		}

	case saveResultMsg:
		if msg.err != nil {
			m.notice = "Error saving: " + msg.err.Error()
			m.noticeErr = true
			m.homeMode = true
			m.viewingSession = nil
			return m, m.cmdLoadSessions()
		} else if msg.folder != "" {
			m.notice = "Saved — generating title & summary..."
			m.noticeErr = false
			m.homeMode = true
			m.viewingSession = nil
			return m, tea.Batch(m.cmdLoadSessions(), m.cmdGenerateMeta(msg.folder, msg.transcript))
		} else {
			m.notice = "Session ended (nothing to save)"
			m.noticeErr = false
			m.homeMode = true
			m.viewingSession = nil
			return m, m.cmdLoadSessions()
		}

	case metaGeneratedMsg:
		if msg.err != nil {
			m.notice = "Saved (could not generate title: " + msg.err.Error() + ")"
			m.noticeErr = false
		} else {
			m.notice = "\"" + msg.title + "\""
			m.noticeErr = false
		}
		return m, m.cmdLoadSessions()

	case chatResponseMsg:
		m.chatLoading = false
		content := msg.content
		if msg.err != nil {
			content = "Error: " + msg.err.Error()
		}
		m.chatMsgs = append(m.chatMsgs, chat.Message{Role: "assistant", Content: content})
		if m.viewingSession != nil && m.viewingSession.Folder != "" {
			_ = m.mgr.Storage.SaveChat(m.viewingSession.Folder, m.chatMsgs)
		}

	case tea.KeyMsg:
		if m.confirmDelete {
			return m.handleConfirmDeleteKey(msg)
		}
		if m.templateOpen {
			return m.handleTemplateKey(msg)
		}
		if m.chatOpen {
			return m.handleChatKey(msg)
		}
		return m.handleKey(msg)
	}

	return m, nil
}

func (m *Model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	// Home screen navigation
	if m.homeMode {
		switch msg.String() {
		case "up", "k":
			if m.viewingSession != nil {
				m.scroll++
			} else if m.sessionIdx > 0 {
				m.sessionIdx--
			}
		case "down", "j":
			if m.viewingSession != nil {
				if m.scroll > 0 {
					m.scroll--
				}
			} else if m.sessionIdx < len(m.sessions)-1 {
				m.sessionIdx++
			}
		case "enter":
			if m.sessionIdx < len(m.sessions) {
				s := m.sessions[m.sessionIdx]
				m.viewingSession = &s
				text := m.mgr.Storage.LoadTranscript(s.Folder)
				m.viewTranscriptRaw = text
				m.viewLines = strings.Split(text, "\n")
				m.chatMsgs = nil // fresh chat for each session
			}
		case "esc":
			m.viewingSession = nil
			m.chatOpen = false
		case "c", "C":
			if m.viewingSession == nil {
				return m, nil
			}
			if m.openAIKey == "" {
				m.notice = "Set OPENAI_API_KEY to enable chat"
				m.noticeErr = true
				return m, nil
			}
			m.chatOpen = !m.chatOpen
			if m.chatOpen {
				m.chatScroll = 0
				m.chatInput.Focus()
				return m, textinput.Blink
			}
			m.chatInput.Blur()
			return m, nil
		case "n", "N":
			m.homeMode = false
			m.viewingSession = nil
			m.lines = nil
			m.partialIdx = -1
			m.duration = "00:00:00"
			m.notice = ""
			return m, m.cmdStartSession()
		case "d", "D":
			if m.sessionIdx < len(m.sessions) && m.viewingSession == nil {
				m.confirmDelete = true
			}
		case "q", "Q", "ctrl+c":
			return m, tea.Quit
		}
		return m, nil
	}

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

func (m *Model) handleTemplateKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "esc":
		m.templateOpen = false
	case "up", "k":
		if m.templateIdx > 0 {
			m.templateIdx--
		}
	case "down", "j":
		if m.templateIdx < len(m.templates)-1 {
			m.templateIdx++
		}
	case "enter":
		if m.templateIdx < len(m.templates) {
			t := m.templates[m.templateIdx]
			m.templateOpen = false
			// Show the template name in chat history, but send the full prompt to the LLM
			m.chatMsgs = append(m.chatMsgs, chat.Message{Role: "user", Content: t.Name})
			m.chatLoading = true
			m.chatScroll = 0
			return m, m.cmdAsk(t.Prompt)
		}
	}
	return m, nil
}

func (m *Model) handleConfirmDeleteKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "y", "Y":
		m.confirmDelete = false
		if m.sessionIdx < len(m.sessions) {
			folder := m.sessions[m.sessionIdx].Folder
			_ = os.RemoveAll(folder)
			// Adjust index so it doesn't go out of bounds
			if m.sessionIdx >= len(m.sessions)-1 && m.sessionIdx > 0 {
				m.sessionIdx--
			}
		}
		return m, m.cmdLoadSessions()
	case "n", "N", "esc":
		m.confirmDelete = false
	}
	return m, nil
}

func (m *Model) overlayConfirmDelete(base string) string {
	if m.sessionIdx >= len(m.sessions) {
		return base
	}
	name := m.sessions[m.sessionIdx].Title
	if name == "" {
		name = m.sessions[m.sessionIdx].Name
	}
	if name == "" {
		name = "this recording"
	}

	label := `Delete "` + name + `"?`
	hint := "  [Y]es  •  [N]o  "
	// w = inner content width (what sits between │ and │, excluding the spaces)
	w := len([]rune(label))
	if hw := len([]rune(hint)); hw > w {
		w = hw
	}
	// border dashes = inner width + 2 spaces (one each side)
	border := strings.Repeat("─", w+2)
	pad := func(s string) string {
		r := []rune(s)
		if len(r) < w {
			s += strings.Repeat(" ", w-len(r))
		}
		return "│ " + s + " │"
	}

	modalLines := []string{
		"┌" + border + "┐",
		pad(label),
		pad(""),
		pad(hint),
		"└" + border + "┘",
	}

	modalH := len(modalLines)
	boxW := len([]rune(modalLines[0]))
	startRow := (m.height - modalH) / 2
	startCol := (m.width - boxW) / 2
	if startRow < 0 {
		startRow = 0
	}
	if startCol < 0 {
		startCol = 0
	}

	baseLines := strings.Split(base, "\n")
	for len(baseLines) < startRow+modalH {
		baseLines = append(baseLines, "")
	}
	for i, ml := range modalLines {
		row := startRow + i
		if row >= len(baseLines) {
			break
		}
		plain := stripANSI(baseLines[row])
		runes := []rune(plain)
		var left string
		if startCol <= len(runes) {
			left = string(runes[:startCol])
		} else {
			left = plain + strings.Repeat(" ", startCol-len(runes))
		}
		baseLines[row] = left + ml
	}
	return strings.Join(baseLines, "\n")
}

func (m *Model) handleChatKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "esc":
		m.chatOpen = false
		m.chatInput.Blur()
		return m, nil
	case "ctrl+c":
		m.chatOpen = false
		m.chatInput.Blur()
		return m, nil
	case "?":
		if len(m.templates) > 0 && m.chatInput.Value() == "" {
			m.templateOpen = true
			m.templateIdx = 0
			return m, nil
		}
		var tiCmd tea.Cmd
		m.chatInput, tiCmd = m.chatInput.Update(msg)
		return m, tiCmd
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
	// Snapshot transcript before the session is torn down
	transcript := ""
	if sess := m.mgr.CurrentSession(); sess != nil {
		transcript = sess.Transcript.ToPlainText()
	}
	return func() tea.Msg {
		folder, err := m.mgr.Finish()
		return saveResultMsg{folder: folder, transcript: transcript, err: err}
	}
}

func (m *Model) cmdGenerateMeta(folder, transcript string) tea.Cmd {
	if m.openAIKey == "" || transcript == "" {
		return nil
	}
	client := chat.New(m.openAIKey, m.chatModel)
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		meta, err := client.GenerateMeta(ctx, transcript)
		if err != nil {
			return metaGeneratedMsg{folder: folder, err: err}
		}
		_ = m.mgr.Storage.SaveMeta(folder, meta.Title, meta.Summary)
		return metaGeneratedMsg{folder: folder, title: meta.Title, summary: meta.Summary}
	}
}

func (m *Model) cmdAsk(userMsg string) tea.Cmd {
	// Snapshot on UI goroutine before launching background work
	transcript := ""
	if sess := m.mgr.CurrentSession(); sess != nil {
		transcript = sess.Transcript.ToPlainText()
	} else if m.viewTranscriptRaw != "" {
		transcript = m.viewTranscriptRaw
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

	base := m.renderBaseView()

	if m.templateOpen {
		return m.overlayTemplateModal(base)
	}
	if m.confirmDelete {
		return m.overlayConfirmDelete(base)
	}

	return base
}

func (m *Model) renderBaseView() string {
	if m.selectingMic {
		return m.viewMicSelect()
	}

	if m.homeMode {
		return m.viewHome()
	}

	var b strings.Builder
	b.WriteString(m.renderHeader())
	b.WriteByte('\n')

	if m.chatOpen {
		transcriptW := (m.width * 40) / 100
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
		if msg.Role == "user" {
			allLines = append(allLines, userStyle.Render("You:"))
			for _, l := range wordWrap(msg.Content, innerWidth-2) {
				allLines = append(allLines, "  "+l)
			}
		} else {
			allLines = append(allLines, aiStyle.Render("AI:"))
			rendered := msg.Content
			if m.mdRenderer != nil {
				if out, err := m.mdRenderer.Render(msg.Content); err == nil {
					rendered = out
				}
			}
			for _, l := range strings.Split(strings.TrimRight(rendered, "\n"), "\n") {
				allLines = append(allLines, l)
			}
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
	b.WriteString("│ " + truncatePad(dimStyle.Render("Enter to send  •  ? for templates  •  Esc to close"), innerWidth) + " │\n")
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
	return "  " + strings.Join(parts, "  │  ")
}

func (m *Model) viewHome() string {
	var b strings.Builder

	// Header
	b.WriteString(m.renderHeader())
	b.WriteByte('\n')

	// Content pane
	available := m.paneHeight()
	innerWidth := m.width - 4
	if innerWidth < 20 {
		innerWidth = 20
	}
	var contentLines []string

	if m.viewingSession != nil {
		s := m.viewingSession
		date := ""
		if len(s.StartTime) >= 10 {
			date = s.StartTime[:10]
		}
		header := speakerStyle.Render(s.Name) + "  " +
			dimStyle.Render(fmt.Sprintf("%s  •  %s  •  %d words", date, formatDuration(s.DurationSecs), s.WordCount))
		contentLines = append(contentLines, header, "")
		for _, l := range m.viewLines {
			for _, wl := range wordWrap(l, innerWidth) {
				contentLines = append(contentLines, wl)
			}
		}
	} else {
		if len(m.sessions) == 0 {
			contentLines = append(contentLines, dimStyle.Render("No recordings yet. Press N to start a new session."))
		} else {
			contentLines = append(contentLines, dimStyle.Render("Past recordings:"), "")
			for i, s := range m.sessions {
				cursor := "  "
				ns := lipgloss.NewStyle()
				if i == m.sessionIdx {
					cursor = "> "
					ns = ns.Bold(true).Foreground(lipgloss.Color("12"))
				}
				date := ""
				if len(s.StartTime) >= 10 {
					date = s.StartTime[:10]
				}
				name := s.Title
				if name == "" {
					name = s.Name
				}
				if lipgloss.Width(name) > 35 {
					name = string([]rune(name)[:34]) + "…"
				}
				meta := dimStyle.Render(fmt.Sprintf("  %s  %s  %d words  %d speakers",
					date, formatDuration(s.DurationSecs), s.WordCount, s.SpeakerCount))
				contentLines = append(contentLines, cursor+ns.Render(name)+meta)
			}
		}
	}

	// Scroll windowing
	total := len(contentLines)
	end := total - m.scroll
	if end < 0 {
		end = 0
	}
	start := end - available
	if start < 0 {
		start = 0
	}
	// When viewing session list, keep selected item visible
	if m.viewingSession == nil && len(m.sessions) > 0 {
		// +2 for the header lines
		selLine := m.sessionIdx + 2
		if selLine < start {
			start = selLine
			end = start + available
			if end > total {
				end = total
			}
		} else if selLine >= end {
			end = selLine + 1
			start = end - available
			if start < 0 {
				start = 0
			}
		}
	}
	visible := contentLines[start:end]

	// Build the main (left) pane content
	mainPane := func(w int) string {
		iw := w - 4
		if iw < 10 {
			iw = 10
		}
		bl := strings.Repeat("─", w-2)
		bk := strings.Repeat(" ", iw)
		var pb strings.Builder
		pb.WriteString("┌" + bl + "┐\n")
		for i := 0; i < available; i++ {
			if i < len(visible) {
				pb.WriteString("│ " + truncatePad(visible[i], iw) + " │\n")
			} else {
				pb.WriteString("│ " + bk + " │\n")
			}
		}
		pb.WriteString("└" + bl + "┘")
		return pb.String()
	}

	if m.chatOpen && m.viewingSession != nil {
		transcriptW := (m.width * 40) / 100
		chatW := m.width - transcriptW
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, mainPane(transcriptW), m.renderChatPane(chatW)))
	} else {
		b.WriteString(mainPane(m.width))
	}
	b.WriteByte('\n')

	// Controls
	if m.viewingSession != nil {
		controls := []string{"[Esc] Back", "[N]ew Session"}
		if m.chatOpen {
			controls = append(controls, "[Esc] Close Chat")
		} else {
			controls = append(controls, "[C]hat")
		}
		b.WriteString("  " + strings.Join(controls, "  │  "))
	} else {
		b.WriteString("  " + strings.Join([]string{"↑/↓ Navigate", "[Enter] View", "[N]ew Session", "[D]elete", "[Q]uit"}, "  │  "))
	}

	return b.String()
}

func formatDuration(secs float64) string {
	total := int(secs)
	h := total / 3600
	m := (total % 3600) / 60
	s := total % 60
	if h > 0 {
		return fmt.Sprintf("%dh%02dm%02ds", h, m, s)
	}
	return fmt.Sprintf("%dm%02ds", m, s)
}

func (m *Model) buildTemplateModalLines() []string {
	maxName := 20
	for _, t := range m.templates {
		if len(t.Name) > maxName {
			maxName = len(t.Name)
		}
	}
	innerW := maxName + 4 // cursor(2) + name + padding(2)
	if innerW < 30 {
		innerW = 30
	}
	if innerW > m.width-6 {
		innerW = m.width - 6
	}
	border := strings.Repeat("─", innerW+2)

	var lines []string
	lines = append(lines, "┌"+border+"┐")
	lines = append(lines, "│ "+truncatePad(titleStyle.Render("Quick Questions"), innerW)+" │")
	lines = append(lines, "├"+border+"┤")
	for i, t := range m.templates {
		var row string
		if i == m.templateIdx {
			row = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("12")).Render("> " + t.Name)
		} else {
			row = "  " + t.Name
		}
		lines = append(lines, "│ "+truncatePad(row, innerW)+" │")
	}
	lines = append(lines, "├"+border+"┤")
	lines = append(lines, "│ "+truncatePad(dimStyle.Render("↑/↓ Navigate  •  Enter to send  •  Esc to close"), innerW)+" │")
	lines = append(lines, "└"+border+"┘")
	return lines
}

func (m *Model) overlayTemplateModal(base string) string {
	modalLines := m.buildTemplateModalLines()
	modalH := len(modalLines)
	boxW := lipgloss.Width(modalLines[0])

	startRow := (m.height - modalH) / 2
	startCol := (m.width - boxW) / 2
	if startRow < 0 {
		startRow = 0
	}
	if startCol < 0 {
		startCol = 0
	}

	baseLines := strings.Split(base, "\n")
	for len(baseLines) < startRow+modalH {
		baseLines = append(baseLines, "")
	}

	for i, ml := range modalLines {
		row := startRow + i
		if row >= len(baseLines) {
			break
		}
		// Keep visible characters to the left of the modal, then place modal line
		plain := stripANSI(baseLines[row])
		runes := []rune(plain)
		var left string
		if startCol <= len(runes) {
			left = string(runes[:startCol])
		} else {
			left = plain + strings.Repeat(" ", startCol-len(runes))
		}
		baseLines[row] = left + ml
	}
	return strings.Join(baseLines, "\n")
}

// stripANSI removes ANSI escape sequences to allow safe column-based slicing.
func stripANSI(s string) string {
	var out strings.Builder
	inEsc := false
	for _, r := range s {
		if r == '\x1b' {
			inEsc = true
			continue
		}
		if inEsc {
			if r == 'm' {
				inEsc = false
			}
			continue
		}
		out.WriteRune(r)
	}
	return out.String()
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
