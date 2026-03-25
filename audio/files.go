package audio

import (
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	mp3 "github.com/hajimehoshi/go-mp3"
)

// ReadAudioFile reads a WAV or MP3 file and returns PCM16 mono data and sample rate.
func ReadAudioFile(path string) ([]byte, int, error) {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".wav":
		return readWAV(path)
	case ".mp3":
		return readMP3(path)
	default:
		return nil, 0, fmt.Errorf("unsupported audio format %q (supported: .wav, .mp3)", filepath.Ext(path))
	}
}

func readWAV(path string) ([]byte, int, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, 0, err
	}
	defer f.Close()

	var riffID [4]byte
	if _, err := io.ReadFull(f, riffID[:]); err != nil {
		return nil, 0, fmt.Errorf("read RIFF: %w", err)
	}
	if string(riffID[:]) != "RIFF" {
		return nil, 0, fmt.Errorf("not a WAV file (missing RIFF header)")
	}
	var chunkSize uint32
	if err := binary.Read(f, binary.LittleEndian, &chunkSize); err != nil {
		return nil, 0, err
	}
	var waveID [4]byte
	if _, err := io.ReadFull(f, waveID[:]); err != nil {
		return nil, 0, err
	}
	if string(waveID[:]) != "WAVE" {
		return nil, 0, fmt.Errorf("not a WAV file (missing WAVE marker)")
	}

	var sampleRate, numChannels, bitsPerSample int
	var pcmData []byte

	for {
		var id [4]byte
		if _, err := io.ReadFull(f, id[:]); err == io.EOF || err == io.ErrUnexpectedEOF {
			break
		} else if err != nil {
			return nil, 0, err
		}
		var size uint32
		if err := binary.Read(f, binary.LittleEndian, &size); err != nil {
			return nil, 0, err
		}
		switch string(id[:]) {
		case "fmt ":
			var audioFormat uint16
			binary.Read(f, binary.LittleEndian, &audioFormat)
			var ch uint16
			binary.Read(f, binary.LittleEndian, &ch)
			numChannels = int(ch)
			var sr uint32
			binary.Read(f, binary.LittleEndian, &sr)
			sampleRate = int(sr)
			f.Seek(4, io.SeekCurrent) // byte rate + block align
			var bps uint16
			binary.Read(f, binary.LittleEndian, &bps)
			bitsPerSample = int(bps)
			if remaining := int64(size) - 16; remaining > 0 {
				f.Seek(remaining, io.SeekCurrent)
			}
		case "data":
			buf := make([]byte, size)
			if _, err := io.ReadFull(f, buf); err != nil {
				return nil, 0, fmt.Errorf("read data chunk: %w", err)
			}
			pcmData = buf
		default:
			f.Seek(int64(size), io.SeekCurrent)
		}
	}

	if sampleRate == 0 {
		return nil, 0, fmt.Errorf("WAV fmt chunk not found")
	}
	if len(pcmData) == 0 {
		return nil, 0, fmt.Errorf("WAV data chunk not found")
	}
	if bitsPerSample != 16 {
		return nil, 0, fmt.Errorf("only 16-bit WAV supported (got %d-bit)", bitsPerSample)
	}

	mono, err := toMono(pcmData, numChannels)
	return mono, sampleRate, err
}

func readMP3(path string) ([]byte, int, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, 0, err
	}
	defer f.Close()

	dec, err := mp3.NewDecoder(f)
	if err != nil {
		return nil, 0, fmt.Errorf("decode mp3: %w", err)
	}
	// go-mp3 always outputs stereo PCM16 little-endian
	raw, err := io.ReadAll(dec)
	if err != nil {
		return nil, 0, fmt.Errorf("read mp3 audio: %w", err)
	}
	mono, err := toMono(raw, 2)
	if err != nil {
		return nil, 0, err
	}
	return mono, dec.SampleRate(), nil
}

// toMono converts interleaved stereo PCM16 to mono by averaging channels.
// Mono data is returned unchanged.
func toMono(pcm []byte, channels int) ([]byte, error) {
	if channels == 1 {
		return pcm, nil
	}
	if channels != 2 {
		return nil, fmt.Errorf("unsupported channel count: %d", channels)
	}
	mono := make([]byte, len(pcm)/2)
	for i := 0; i+3 < len(pcm); i += 4 {
		l := int32(int16(binary.LittleEndian.Uint16(pcm[i:])))
		r := int32(int16(binary.LittleEndian.Uint16(pcm[i+2:])))
		binary.LittleEndian.PutUint16(mono[i/2:], uint16(int16((l+r)/2)))
	}
	return mono, nil
}
