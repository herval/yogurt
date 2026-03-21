package ui

import (
	"fmt"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"

	"github.com/herval/yogurtgo/audio"
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
type permissionResultMsg struct{ err error }

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
	borderStyle  = lipgloss.NewStyle().Border(lipgloss.NormalBorder()).BorderForeground(lipgloss.Color("4"))
	noticeStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("11"))
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
	partialIdx int // index of current partial line (-1 if none)
	scroll     int // how many lines scrolled up from bottom
	width      int
	height     int
	notice     string
	noticeErr  bool

	// device selection
	selectingMic bool
	micListIdx   int
}

func New(mgr *session.Manager, devices []audio.Device) *Model {
	return &Model{
		mgr:        mgr,
		devices:    devices,
		status:     session.StatusIdle,
		partialIdx: -1,
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
		if m.scroll == 0 {
			// auto-scroll to bottom: no-op, we always render from end
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

	case permissionResultMsg:
		if msg.err != nil {
			m.notice = msg.err.Error()
			m.noticeErr = true
		} else {
			m.notice = "Microphone access granted"
			m.noticeErr = false
		}

	case tea.KeyMsg:
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

func (m *Model) cmdStartSession() tea.Cmd {
	return func() tea.Msg {
		err := m.mgr.StartSession("")
		if err != nil {
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

// View renders the full TUI.
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
	b.WriteString(m.renderTranscript())
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

func (m *Model) renderTranscript() string {
	// Calculate available height: total - header(1) - newline(1) - statusbar(1) - newline(1) - controls(1) - notice(up to 2)
	available := m.height - 8
	if available < 3 {
		available = 3
	}

	// Build lines to display
	displayLines := m.buildDisplayLines()
	total := len(displayLines)

	// Apply scroll
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
	inner := strings.Repeat(" ", m.width-2)
	border := strings.Repeat("─", m.width-2)
	b.WriteString("┌" + border + "┐\n")
	for i := 0; i < available; i++ {
		if i < len(visible) {
			line := visible[i]
			padded := truncatePad(line, m.width-4)
			b.WriteString("│ " + padded + " │\n")
		} else {
			b.WriteString("│ " + inner + " │\n")
		}
	}
	b.WriteString("└" + border + "┘")
	return b.String()
}

// buildDisplayLines produces rendered strings for each transcript line.
func (m *Model) buildDisplayLines() []string {
	innerWidth := m.width - 4 // 2 for borders + 2 for padding
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

		// Word-wrap the transcript text
		wrapped := wordWrap(seg.Text, innerWidth-2) // -2 for the "  " indent
		for _, line := range wrapped {
			if l.partial {
				out = append(out, "  "+dimStyle.Render(line))
			} else {
				out = append(out, "  "+line)
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
	var lines []string
	words := strings.Fields(text)
	if len(words) == 0 {
		return []string{""}
	}

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
	parts := []string{}

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

// truncatePad truncates or pads a string to exactly w visible characters.
func truncatePad(s string, w int) string {
	// Strip ANSI for width calculation isn't trivial; use lipgloss width
	visible := lipgloss.Width(s)
	if visible > w {
		// crude truncation
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

// --- Adapter: wire manager callbacks to bubbletea messages ---

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
