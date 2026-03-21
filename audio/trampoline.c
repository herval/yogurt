// trampoline.c — bridges CGo-exported Go functions into C function pointers.
// This file can include _cgo_export.h which is generated at build time.
#include "_cgo_export.h"

void yogurtGoAudioCB(const void *data, int len, void *ctx) {
    goAudioCallback((void *)data, len, ctx);
}

void yogurtGoPermCB(int granted, void *ctx) {
    goPermissionCallback(granted, ctx);
}
