package audio

import (
	"encoding/binary"
	"os"
)

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
	write(uint32(16))
	write(uint16(1)) // PCM
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
