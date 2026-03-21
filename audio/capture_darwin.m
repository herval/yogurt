// AVFoundation audio capture for macOS
// Captures PCM16 audio at 16kHz mono and delivers it via a C callback.

#import <AVFoundation/AVFoundation.h>
#import <CoreAudio/CoreAudio.h>
#import <Accelerate/Accelerate.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Callback type: called with (PCM16 data, byte length, user context)
typedef void (*YogurtAudioCallback)(const void *data, int len, void *ctx);

// --- Permission ---

typedef void (*YogurtPermissionCallback)(int granted, void *ctx);

void yogurt_request_permission(YogurtPermissionCallback cb, void *ctx) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL granted) {
        if (cb) cb(granted ? 1 : 0, ctx);
    }];
}

int yogurt_authorization_status(void) {
    AVAuthorizationStatus status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
    // 3 = AVAuthorizationStatusAuthorized
    return (int)status;
}

// --- Device enumeration ---

int yogurt_list_devices(char **names, int *indices, int maxDevices) {
    NSArray<AVCaptureDevice *> *devices;
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    if (@available(macOS 14.0, *)) {
        AVCaptureDeviceDiscoverySession *session = [AVCaptureDeviceDiscoverySession
            discoverySessionWithDeviceTypes:@[AVCaptureDeviceTypeMicrophone]
            mediaType:AVMediaTypeAudio
            position:AVCaptureDevicePositionUnspecified];
        devices = session.devices;
    } else if (@available(macOS 10.15, *)) {
        AVCaptureDeviceDiscoverySession *session = [AVCaptureDeviceDiscoverySession
            discoverySessionWithDeviceTypes:@[AVCaptureDeviceTypeBuiltInMicrophone,
                                              AVCaptureDeviceTypeExternalUnknown]
            mediaType:AVMediaTypeAudio
            position:AVCaptureDevicePositionUnspecified];
        devices = session.devices;
    } else {
        devices = [AVCaptureDevice devicesWithMediaType:AVMediaTypeAudio];
    }
#pragma clang diagnostic pop

    int count = 0;
    for (AVCaptureDevice *dev in devices) {
        if (count >= maxDevices) break;
        const char *name = [[dev localizedName] UTF8String];
        names[count] = strdup(name ? name : "Unknown");
        indices[count] = count; // use array index as device index
        count++;
    }
    return count;
}

void yogurt_free_device_names(char **names, int count) {
    for (int i = 0; i < count; i++) {
        if (names[i]) free(names[i]);
    }
}

// --- Capture session ---

@interface YogurtAudioDelegate : NSObject <AVCaptureAudioDataOutputSampleBufferDelegate>
@property (nonatomic, assign) YogurtAudioCallback callback;
@property (nonatomic, assign) void *ctx;
@property (nonatomic, assign) int targetSampleRate;
@property (nonatomic, assign) BOOL paused;
@end

@implementation YogurtAudioDelegate

- (void)captureOutput:(AVCaptureOutput *)output
didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
       fromConnection:(AVCaptureConnection *)connection {
    if (self.paused) return;

    CMFormatDescriptionRef fmt = CMSampleBufferGetFormatDescription(sampleBuffer);
    const AudioStreamBasicDescription *asbd = CMAudioFormatDescriptionGetStreamBasicDescription(fmt);
    if (!asbd) return;

    // Get audio buffer list
    CMBlockBufferRef blockBuffer = NULL;
    AudioBufferList audioBufferList;
    CMItemCount numSamples = CMSampleBufferGetNumSamples(sampleBuffer);

    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer,
        NULL,
        &audioBufferList,
        sizeof(audioBufferList),
        NULL,
        NULL,
        kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
        &blockBuffer
    );

    if (status != noErr || audioBufferList.mNumberBuffers == 0) return;

    AudioBuffer *buf = &audioBufferList.mBuffers[0];
    if (!buf->mData || buf->mDataByteSize == 0) {
        if (blockBuffer) CFRelease(blockBuffer);
        return;
    }

    float srcRate = asbd->mSampleRate;
    float dstRate = (float)self.targetSampleRate;

    // Determine how many PCM16 output samples we expect
    // Input: buf->mData, format depends on asbd
    // We'll convert to float first, then resample, then to int16

    int numInputFrames = (int)numSamples;
    int numChannels = (int)asbd->mChannelsPerFrame;
    if (numChannels < 1) numChannels = 1;

    // Convert source to float mono
    float *floatMono = (float *)malloc(numInputFrames * sizeof(float));
    if (!floatMono) {
        if (blockBuffer) CFRelease(blockBuffer);
        return;
    }

    if (asbd->mFormatFlags & kAudioFormatFlagIsFloat) {
        // Float32 input
        float *src = (float *)buf->mData;
        int totalSamples = buf->mDataByteSize / sizeof(float);
        int framesAvail = totalSamples / numChannels;
        if (framesAvail < numInputFrames) numInputFrames = framesAvail;
        for (int i = 0; i < numInputFrames; i++) {
            float sum = 0;
            for (int c = 0; c < numChannels; c++) {
                sum += src[i * numChannels + c];
            }
            floatMono[i] = sum / numChannels;
        }
    } else {
        // Int16 or Int32 input — convert to float
        if (asbd->mBitsPerChannel == 16) {
            int16_t *src = (int16_t *)buf->mData;
            int totalSamples = buf->mDataByteSize / sizeof(int16_t);
            int framesAvail = totalSamples / numChannels;
            if (framesAvail < numInputFrames) numInputFrames = framesAvail;
            for (int i = 0; i < numInputFrames; i++) {
                float sum = 0;
                for (int c = 0; c < numChannels; c++) {
                    sum += src[i * numChannels + c] / 32768.0f;
                }
                floatMono[i] = sum / numChannels;
            }
        } else if (asbd->mBitsPerChannel == 32) {
            int32_t *src = (int32_t *)buf->mData;
            int totalSamples = buf->mDataByteSize / sizeof(int32_t);
            int framesAvail = totalSamples / numChannels;
            if (framesAvail < numInputFrames) numInputFrames = framesAvail;
            for (int i = 0; i < numInputFrames; i++) {
                float sum = 0;
                for (int c = 0; c < numChannels; c++) {
                    sum += src[i * numChannels + c] / 2147483648.0f;
                }
                floatMono[i] = sum / numChannels;
            }
        } else {
            // Unsupported format, skip
            free(floatMono);
            if (blockBuffer) CFRelease(blockBuffer);
            return;
        }
    }

    // Resample from srcRate to dstRate using vDSP
    double ratio = dstRate / srcRate;
    int numOutputFrames = (int)(numInputFrames * ratio + 0.5);
    if (numOutputFrames <= 0) {
        free(floatMono);
        if (blockBuffer) CFRelease(blockBuffer);
        return;
    }

    float *resampled = (float *)malloc(numOutputFrames * sizeof(float));
    if (!resampled) {
        free(floatMono);
        if (blockBuffer) CFRelease(blockBuffer);
        return;
    }

    // Simple linear interpolation resampling
    for (int i = 0; i < numOutputFrames; i++) {
        double srcIdx = i / ratio;
        int idx0 = (int)srcIdx;
        int idx1 = idx0 + 1;
        double frac = srcIdx - idx0;
        if (idx1 >= numInputFrames) idx1 = numInputFrames - 1;
        if (idx0 >= numInputFrames) idx0 = numInputFrames - 1;
        resampled[i] = (float)(floatMono[idx0] * (1.0 - frac) + floatMono[idx1] * frac);
    }
    free(floatMono);

    // Convert to PCM16
    int16_t *pcm16 = (int16_t *)malloc(numOutputFrames * sizeof(int16_t));
    if (!pcm16) {
        free(resampled);
        if (blockBuffer) CFRelease(blockBuffer);
        return;
    }

    for (int i = 0; i < numOutputFrames; i++) {
        float s = resampled[i];
        if (s > 1.0f) s = 1.0f;
        if (s < -1.0f) s = -1.0f;
        pcm16[i] = (int16_t)(s * 32767.0f);
    }
    free(resampled);

    if (self.callback) {
        self.callback(pcm16, numOutputFrames * sizeof(int16_t), self.ctx);
    }
    free(pcm16);

    if (blockBuffer) CFRelease(blockBuffer);
}

@end

typedef struct {
    void *session;   // CFTypeRef to AVCaptureSession (manual retain)
    void *delegate;  // CFTypeRef to YogurtAudioDelegate (manual retain)
    dispatch_queue_t queue;
} YogurtCaptureState;

void *yogurt_start_capture(int deviceIndex, int targetSampleRate,
                            YogurtAudioCallback callback, void *ctx) {
    // Get device list
    NSArray<AVCaptureDevice *> *devices;
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    if (@available(macOS 14.0, *)) {
        AVCaptureDeviceDiscoverySession *ds = [AVCaptureDeviceDiscoverySession
            discoverySessionWithDeviceTypes:@[AVCaptureDeviceTypeMicrophone]
            mediaType:AVMediaTypeAudio
            position:AVCaptureDevicePositionUnspecified];
        devices = ds.devices;
    } else if (@available(macOS 10.15, *)) {
        AVCaptureDeviceDiscoverySession *ds = [AVCaptureDeviceDiscoverySession
            discoverySessionWithDeviceTypes:@[AVCaptureDeviceTypeBuiltInMicrophone,
                                              AVCaptureDeviceTypeExternalUnknown]
            mediaType:AVMediaTypeAudio
            position:AVCaptureDevicePositionUnspecified];
        devices = ds.devices;
    } else {
        devices = [AVCaptureDevice devicesWithMediaType:AVMediaTypeAudio];
    }
#pragma clang diagnostic pop

    AVCaptureDevice *dev = nil;
    if (deviceIndex >= 0 && deviceIndex < (int)devices.count) {
        dev = devices[deviceIndex];
    } else {
        dev = [AVCaptureDevice defaultDeviceWithMediaType:AVMediaTypeAudio];
    }
    if (!dev) return NULL;

    NSError *error = nil;
    AVCaptureDeviceInput *input = [AVCaptureDeviceInput deviceInputWithDevice:dev error:&error];
    if (!input) return NULL;

    AVCaptureSession *session = [[AVCaptureSession alloc] init];
    if (![session canAddInput:input]) return NULL;
    [session addInput:input];

    AVCaptureAudioDataOutput *output = [[AVCaptureAudioDataOutput alloc] init];

    YogurtAudioDelegate *delegate = [[YogurtAudioDelegate alloc] init];
    delegate.callback = callback;
    delegate.ctx = ctx;
    delegate.targetSampleRate = targetSampleRate;
    delegate.paused = NO;

    dispatch_queue_t queue = dispatch_queue_create("com.yogurt.audio", DISPATCH_QUEUE_SERIAL);
    [output setSampleBufferDelegate:delegate queue:queue];

    if (![session canAddOutput:output]) return NULL;
    [session addOutput:output];

    [session startRunning];

    YogurtCaptureState *state = (YogurtCaptureState *)malloc(sizeof(YogurtCaptureState));
    state->session = (void *)CFBridgingRetain(session);
    state->delegate = (void *)CFBridgingRetain(delegate);
    state->queue = queue;

    return state;
}

void yogurt_pause_capture(void *handle) {
    if (!handle) return;
    YogurtCaptureState *state = (YogurtCaptureState *)handle;
    YogurtAudioDelegate *delegate = (__bridge YogurtAudioDelegate *)(CFTypeRef)state->delegate;
    delegate.paused = YES;
}

void yogurt_resume_capture(void *handle) {
    if (!handle) return;
    YogurtCaptureState *state = (YogurtCaptureState *)handle;
    YogurtAudioDelegate *delegate = (__bridge YogurtAudioDelegate *)(CFTypeRef)state->delegate;
    delegate.paused = NO;
}

void yogurt_stop_capture(void *handle) {
    if (!handle) return;
    YogurtCaptureState *state = (YogurtCaptureState *)handle;
    // Transfer ownership back to ARC so objects are released on scope exit
    AVCaptureSession *session = (__bridge_transfer AVCaptureSession *)(CFTypeRef)state->session;
    YogurtAudioDelegate *delegate = (__bridge_transfer YogurtAudioDelegate *)(CFTypeRef)state->delegate;
    (void)delegate;
    [session stopRunning];
    [session beginConfiguration];
    for (AVCaptureInput *inp in session.inputs) [session removeInput:inp];
    for (AVCaptureOutput *out in session.outputs) [session removeOutput:out];
    [session commitConfiguration];
    free(state);
}
