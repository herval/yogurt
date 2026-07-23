```
__   __ ___   ____  _   _ ____  _____
\ \ / // _ \ / ___|| | | |  _ \|_   _|
 \ V /| | | | |  _ | | | | |_) | | |
  | | | |_| | |_| || |_| |  _ <  | |
  |_|  \___/ \____| \___/|_| \_\ |_|
```

Real-time meeting recorder and transcriber for macOS.

Records your microphone plus system audio (so remote meeting participants
are captured too), transcribes with speaker detection, and lets you chat
with the transcript using AI. With ElevenLabs, recordings are stereo — your
mic on the left, the meeting on the right — so the transcript labels your
lines as "You" and remote speech by channel. Mono providers (AssemblyAI,
whisper) get a mixed-down stream with acoustic diarization instead.

---

## Requirements

- macOS (uses AVFoundation for audio)
- An STT provider API key (transcription) — AssemblyAI by default
- An LLM provider API key (chat, optional)

## Setup

```bash
cd yogurt
cp .env.example .env        # add your API keys
make build                  # builds yogurt.app + ./yogurt symlink
./yogurt
```

> `make build` produces an `.app` bundle (with `./yogurt` symlinked to the
> inner binary) rather than a bare binary: macOS TCC refuses microphone
> access to unbundled executables — they die with `Abort trap: 6`.

Building requires a Rust toolchain (`rustup`). `make run ARGS="--list-devices"`
builds and runs in one step.

## Environment variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `STT_MODEL` | Yes* | `assemblyai/universal-streaming-multilingual` | Speech-to-text provider/model |
| `LLM_MODEL` | No | `openai/gpt-4o-mini` | LLM provider/model for chat |
| `ASSEMBLYAI_API_KEY` | Yes* | — | AssemblyAI key (if using AssemblyAI) |
| `ELEVENLABS_API_KEY` | — | — | ElevenLabs key (if using ElevenLabs) |
| `OPENAI_API_KEY` | — | — | OpenAI key (if using OpenAI) |
| `GEMINI_API_KEY` | — | — | Gemini key (if using Gemini) |
| `ANTHROPIC_API_KEY` | — | — | Anthropic key (if using Anthropic) |
| `YOGURT_SESSIONS_DIR` | No | `./sessions` | Where to save recordings |
| `YOGURT_SAMPLE_RATE` | No | `16000` | Audio sample rate |
| `YOGURT_AUDIO_DEVICE` | No | default mic | Device index or name |
| `YOGURT_SYSTEM_AUDIO` | No | `1` | Set `0` to record mic only (no system audio) |
| `YOGURT_ELEVENLABS_LIVE_SECS` | No | `10` | Live transcription window for ElevenLabs (`0` = only on stop) |

\* Required for the configured STT provider. `ASSEMBLYAI_API_KEY` is required by default.

### Supported providers

**STT:** `assemblyai` (streaming, default), `elevenlabs` (batch), `whisper` (local — run `make setup MODEL=base` once to download the model and build with whisper support; requires `cmake`)

**LLM:** `openai` (default), `gemini`, `anthropic`

**Examples:**
```bash
STT_MODEL=assemblyai/universal-streaming-multilingual  ASSEMBLYAI_API_KEY=...
STT_MODEL=elevenlabs/scribe_v1                         ELEVENLABS_API_KEY=...
STT_MODEL=whisper/base                                 # no key, local model

LLM_MODEL=openai/gpt-4o-mini                           OPENAI_API_KEY=...
LLM_MODEL=gemini/gemini-2.0-flash                      GEMINI_API_KEY=...
LLM_MODEL=anthropic/claude-3-5-haiku-20241022          ANTHROPIC_API_KEY=...
```

## Usage

```bash
./yogurt                        # start
./yogurt --list-devices         # list audio input devices
./yogurt --device 2             # use a specific device
./yogurt --mic-check            # record 2s and report the input level
./yogurt --sys-check            # capture 3s of system audio and report the level
./yogurt --sessions-dir ~/meetings
```

### Transcribe a file

Pass `--file` to transcribe an existing WAV file instead of recording live.
No TUI is launched — results are printed to stdout and saved as a normal session.

```bash
./yogurt --file recording.wav
./yogurt --file recording.wav --name "team standup"
./yogurt --file meeting.wav --sessions-dir ~/meetings
```

The session name defaults to the filename without extension.
Supported formats: `.wav` (16-bit PCM), `.mp3`. Stereo is automatically converted to mono.

## Controls

### Session list

| Key | Action |
|---|---|
| `↑` / `↓` | Navigate sessions |
| `Enter` | View session transcript |
| `N` | Start a new recording |
| `D` | Delete selected session (with confirmation) |
| `Q` | Quit |

### While recording

| Key | Action |
|---|---|
| `P` | Pause / Resume |
| `F` | Finish and save |
| `M` | Change microphone |

### Viewing a transcript

| Key | Action |
|---|---|
| `↑` / `↓` | Scroll transcript |
| `C` | Open / close chat panel |
| `Esc` | Back to session list |

### Chat panel

| Key | Action |
|---|---|
| `Enter` | Send message |
| `?` | Open quick-question templates |
| `↑` / `↓` | Scroll chat history |
| `Esc` | Close chat |

## Sessions

Each recording is saved in `sessions/YYYY-MM-DD_HH-MM-SS_<name>/`:

```
audio.wav          raw audio
transcript.txt     plain-text transcript with speaker labels
transcript.json    full transcript with timestamps
metadata.json      duration, word count, AI-generated title & summary
summary.md         AI-generated title and summary
chat.json          chat history (if you used the chat panel)
```

## Chat templates

Quick-question templates are loaded from `~/.yogurt/chat_templates.json`.
The file is created with defaults on first run. Edit it to add your own:

```json
[
  {
    "name": "My question",
    "prompt": "The full prompt sent to the AI..."
  }
]
```
