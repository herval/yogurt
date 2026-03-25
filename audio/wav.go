package audio

import (
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

// ReadAudioFile reads a WAV or MP3 file and returns PCM16 mono data and sample rate.
func ReadAudioFile(path string) ([]byte, int, error) {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".wav":
		return ReadWAV(path)
	case ".mp3":
		return ReadMP3(path)
	default:
		return nil, 0, fmt.Errorf("unsupported audio format %q (supported: .wav, .mp3)", filepath.Ext(path))
	}
}

// WriteWAV writes PCM16 mono audio to a WAV file.
func WriteWAV(path string, pcm []byte, sampleRate int) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()

	channels := uint16(1)
	bitsPerSample := uint16(16)
	blockAlign := channels * bitsPerSample / 8
	byteRate := uint32(sampleRate) * uint32(blockAlign)
	dataSize := uint32(len(pcm))
	chunkSize := 36 + dataSize

	write := func(v any) {
		_ = binary.Write(f, binary.LittleEndian, v)
	}

	f.WriteString("RIFF")
	write(chunkSize)
	f.WriteString("WAVE")
	f.WriteString("fmt ")
	write(uint32(16))       // subchunk1 size
	write(uint16(1))        // PCM
	write(channels)
	write(uint32(sampleRate))
	write(byteRate)
	write(blockAlign)
	write(bitsPerSample)
	f.WriteString("data")
	write(dataSize)
	f.Write(pcm)

	return nil
}

// ReadWAV reads a WAV file and returns the raw PCM16 mono data and sample rate.
// Stereo is converted to mono by averaging channels.
func ReadWAV(path string) ([]byte, int, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, 0, err
	}
	defer f.Close()

	// RIFF header
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

	var sampleRate int
	var numChannels int
	var bitsPerSample int
	var pcmData []byte

	// Scan chunks
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
			// skip any remaining fmt bytes
			remaining := int64(size) - 16
			if remaining > 0 {
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

	// Convert stereo → mono if needed
	if numChannels == 2 {
		mono := make([]byte, len(pcmData)/2)
		for i := 0; i+3 < len(pcmData); i += 4 {
			l := int32(int16(binary.LittleEndian.Uint16(pcmData[i:])))
			r := int32(int16(binary.LittleEndian.Uint16(pcmData[i+2:])))
			avg := int16((l + r) / 2)
			binary.LittleEndian.PutUint16(mono[i/2:], uint16(avg))
		}
		pcmData = mono
	} else if numChannels != 1 {
		return nil, 0, fmt.Errorf("unsupported channel count: %d", numChannels)
	}

	return pcmData, sampleRate, nil
}
