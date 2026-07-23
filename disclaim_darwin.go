//go:build darwin

package main

/*
#include <spawn.h>
#include <stdlib.h>

extern char **environ;
// Private API (libsystem): makes the spawned process its own "responsible
// process" for TCC. Without it, terminal-launched processes are attributed to
// an ancestor (terminal/tmux daemon), which can't satisfy mic permission.
extern int responsibility_spawnattrs_setdisclaim(posix_spawnattr_t *attrs, int disclaim);

static int spawn_disclaimed(const char *path, char *const argv[], pid_t *pid) {
	posix_spawnattr_t attr;
	int rc = posix_spawnattr_init(&attr);
	if (rc != 0) return rc;
	responsibility_spawnattrs_setdisclaim(&attr, 1);
	rc = posix_spawn(pid, path, NULL, &attr, argv, environ);
	posix_spawnattr_destroy(&attr);
	return rc;
}
*/
import "C"

import (
	"os"
	"os/signal"
	"syscall"
	"unsafe"
)

const disclaimEnv = "YOGURT_DISCLAIMED"

// maybeReexecDisclaimed re-runs the current binary as its own TCC responsible
// process, waits for it, and exits with its status. No-op in the child.
func maybeReexecDisclaimed() {
	if os.Getenv(disclaimEnv) != "" {
		return
	}
	os.Setenv(disclaimEnv, "1")

	exe, err := os.Executable()
	if err != nil {
		return // fall through and hope for the best
	}

	argv := make([]*C.char, len(os.Args)+1)
	for i, a := range os.Args {
		argv[i] = C.CString(a)
	}
	cPath := C.CString(exe)
	defer func() {
		C.free(unsafe.Pointer(cPath))
		for _, p := range argv[:len(os.Args)] {
			C.free(unsafe.Pointer(p))
		}
	}()

	var pid C.pid_t
	if rc := C.spawn_disclaimed(cPath, &argv[0], &pid); rc != 0 {
		return // spawn failed; continue undisclaimed
	}

	// The child owns the terminal now; ignore job-control signals and wait.
	signal.Ignore(os.Interrupt, syscall.SIGTERM)
	var status syscall.WaitStatus
	if _, err := syscall.Wait4(int(pid), &status, 0, nil); err != nil {
		os.Exit(1)
	}
	if status.Signaled() {
		os.Exit(128 + int(status.Signal()))
	}
	os.Exit(status.ExitStatus())
}
