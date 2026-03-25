package audio

import (
	"encoding/binary"
	"fmt"
	"io"
	"os"

	mp3 "github.com/hajimehoshi/go-mp3"
)

// ReadMP3 decodes an MP3 file and returns PCM16 mono data and the sample rate.
// Stereo is converted to mono by averaging channels.
func ReadMP3(path string) ([]byte, int, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, 0, err
	}
	defer f.Close()

	dec, err := mp3.NewDecoder(f)
	if err != nil {
		return nil, 0, fmt.Errorf("decode mp3: %w", err)
	}

	sampleRate := dec.SampleRate()

	// go-mp3 always outputs stereo PCM16 little-endian regardless of source channels.
	raw, err := io.ReadAll(dec)
	if err != nil {
		return nil, 0, fmt.Errorf("read mp3 audio: %w", err)
	}

	// Convert stereo → mono by averaging L and R samples.
	mono := make([]byte, len(raw)/2)
	for i := 0; i+3 < len(raw); i += 4 {
		l := int32(int16(binary.LittleEndian.Uint16(raw[i:])))
		r := int32(int16(binary.LittleEndian.Uint16(raw[i+2:])))
		avg := int16((l + r) / 2)
		binary.LittleEndian.PutUint16(mono[i/2:], uint16(avg))
	}

	return mono, sampleRate, nil
}
