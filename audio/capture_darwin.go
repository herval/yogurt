package audio

/*
#cgo CFLAGS: -x objective-c -fobjc-arc
#cgo LDFLAGS: -framework AVFoundation -framework CoreAudio -framework CoreMedia -framework Accelerate -framework Foundation

#include <stdlib.h>
#include <stdint.h>

typedef void (*YogurtAudioCallback)(const void *data, int len, void *ctx);
typedef void (*YogurtPermissionCallback)(int granted, void *ctx);

extern void  yogurt_request_permission(YogurtPermissionCallback cb, void *ctx);
extern int   yogurt_authorization_status(void);
extern int   yogurt_list_devices(char **names, int *indices, int maxDevices);
extern void  yogurt_free_device_names(char **names, int count);
extern void *yogurt_start_capture(int deviceIndex, int targetSampleRate, YogurtAudioCallback callback, void *ctx);
extern void  yogurt_pause_capture(void *handle);
extern void  yogurt_resume_capture(void *handle);
extern void  yogurt_stop_capture(void *handle);

// Trampolines defined in trampoline.c (can include _cgo_export.h)
extern void yogurtGoAudioCB(const void *data, int len, void *ctx);
extern void yogurtGoPermCB(int granted, void *ctx);

*/
import "C"

import (
	"fmt"
	"sync"
	"unsafe"
)

// audioCallbackRegistry maps handle pointers to Go channels so C callbacks can reach Go.
var (
	callbackMu       sync.Mutex
	callbackRegistry = map[uintptr]chan<- []byte{}
)

//export goAudioCallback
func goAudioCallback(data unsafe.Pointer, length C.int, ctx unsafe.Pointer) {
	key := uintptr(ctx)
	callbackMu.Lock()
	ch, ok := callbackRegistry[key]
	callbackMu.Unlock()
	if !ok || ch == nil {
		return
	}
	n := int(length)
	buf := make([]byte, n)
	copy(buf, (*[1 << 28]byte)(data)[:n:n])
	select {
	case ch <- buf:
	default: // drop if consumer is slow
	}
}

// permissionCallbacks maps context keys to Go channels for permission results.
var (
	permMu       sync.Mutex
	permRegistry = map[uintptr]chan<- bool{}
	permCounter  uintptr
)

//export goPermissionCallback
func goPermissionCallback(granted C.int, ctx unsafe.Pointer) {
	key := uintptr(ctx)
	permMu.Lock()
	ch, ok := permRegistry[key]
	if ok {
		delete(permRegistry, key)
	}
	permMu.Unlock()
	if ok && ch != nil {
		ch <- (granted != 0)
	}
}

// Device represents an audio input device.
type Device struct {
	Index int
	Name  string
}

// ListDevices returns all available audio input devices.
func ListDevices() []Device {
	const maxDevices = 64
	names := make([]*C.char, maxDevices)
	indices := make([]C.int, maxDevices)

	count := int(C.yogurt_list_devices(&names[0], &indices[0], C.int(maxDevices)))
	devices := make([]Device, count)
	for i := 0; i < count; i++ {
		devices[i] = Device{
			Index: int(indices[i]),
			Name:  C.GoString(names[i]),
		}
	}
	C.yogurt_free_device_names(&names[0], C.int(count))
	return devices
}

// AuthorizationStatus returns current mic permission status.
// 3 = authorized, 2 = denied, 1 = restricted, 0 = not determined.
func AuthorizationStatus() int {
	return int(C.yogurt_authorization_status())
}

// RequestPermission requests microphone access and blocks until the user responds.
// Returns nil if granted, error if denied.
func RequestPermission() error {
	ch := make(chan bool, 1)

	permMu.Lock()
	permCounter++
	key := permCounter
	permRegistry[key] = ch
	permMu.Unlock()

	C.yogurt_request_permission(
		(C.YogurtPermissionCallback)(C.yogurtGoPermCB),
		unsafe.Pointer(key),
	)

	granted := <-ch
	if !granted {
		return fmt.Errorf("microphone access denied — enable in System Settings → Privacy & Security → Microphone")
	}
	return nil
}

// Capture manages an AVFoundation audio capture session.
type Capture struct {
	handle unsafe.Pointer
	key    uintptr
}

// Start begins audio capture. Audio chunks (PCM16, 16kHz mono) are sent to ch.
// deviceIndex = -1 uses the default microphone.
func (c *Capture) Start(deviceIndex, sampleRate int, ch chan<- []byte) error {
	if c.handle != nil {
		return fmt.Errorf("capture already started")
	}

	// Register the channel before starting so the first callback can find it.
	callbackMu.Lock()
	// Use the address of ch as a unique key (channel header pointer).
	key := uintptr(unsafe.Pointer(&ch))
	callbackRegistry[key] = ch
	c.key = key
	callbackMu.Unlock()

	handle := C.yogurt_start_capture(
		C.int(deviceIndex),
		C.int(sampleRate),
		(C.YogurtAudioCallback)(C.yogurtGoAudioCB),
		unsafe.Pointer(key),
	)
	if handle == nil {
		callbackMu.Lock()
		delete(callbackRegistry, key)
		callbackMu.Unlock()
		return fmt.Errorf("failed to start audio capture — check microphone permission")
	}
	c.handle = handle
	return nil
}

// Pause stops delivering audio data (capture session remains running).
func (c *Capture) Pause() {
	if c.handle != nil {
		C.yogurt_pause_capture(c.handle)
	}
}

// Resume resumes delivering audio data after a pause.
func (c *Capture) Resume() {
	if c.handle != nil {
		C.yogurt_resume_capture(c.handle)
	}
}

// Stop ends the capture session.
func (c *Capture) Stop() {
	if c.handle == nil {
		return
	}
	C.yogurt_stop_capture(c.handle)
	c.handle = nil

	callbackMu.Lock()
	delete(callbackRegistry, c.key)
	callbackMu.Unlock()
}
