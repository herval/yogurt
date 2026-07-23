//! macOS TCC responsibility disclaim.
//!
//! Terminal-launched processes get TCC-attributed to an ancestor (terminal /
//! tmux daemon) that can't hold mic permission; any run that needs a permission
//! prompt is killed with SIGABRT. Re-exec'ing ourselves with the (private)
//! disclaim spawn attribute makes this process its own "responsible process",
//! so the app's own bundle identity + usage description apply.

#[cfg(target_os = "macos")]
pub fn maybe_reexec_disclaimed() {
    use std::env;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const SENTINEL: &str = "YOGURT_DISCLAIMED";

    unsafe extern "C" {
        // Private API in libSystem.
        fn responsibility_spawnattrs_setdisclaim(
            attrs: *mut libc::posix_spawnattr_t,
            disclaim: libc::c_int,
        ) -> libc::c_int;
    }

    if env::var_os(SENTINEL).is_some() {
        return; // we are the disclaimed child
    }

    let Ok(exe) = env::current_exe() else { return };
    let Ok(c_path) = CString::new(exe.as_os_str().as_bytes()) else {
        return;
    };

    let args: Vec<CString> = env::args_os()
        .filter_map(|a| CString::new(a.as_bytes()).ok())
        .collect();
    let mut argv: Vec<*mut libc::c_char> =
        args.iter().map(|a| a.as_ptr() as *mut libc::c_char).collect();
    argv.push(std::ptr::null_mut());

    // SAFETY: standard posix_spawn usage; the private disclaim call takes the
    // same attr struct and is a no-op-style int setter.
    unsafe {
        env::set_var(SENTINEL, "1");

        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return;
        }
        responsibility_spawnattrs_setdisclaim(&mut attr, 1);

        let mut pid: libc::pid_t = 0;
        let rc = libc::posix_spawn(
            &mut pid,
            c_path.as_ptr(),
            std::ptr::null(),
            &attr,
            argv.as_ptr(),
            environ_ptr(),
        );
        libc::posix_spawnattr_destroy(&mut attr);
        if rc != 0 {
            return; // spawn failed; continue undisclaimed
        }

        // The child owns the terminal now; ignore job-control signals and wait.
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);

        let mut status: libc::c_int = 0;
        if libc::waitpid(pid, &mut status, 0) < 0 {
            std::process::exit(1);
        }
        if libc::WIFSIGNALED(status) {
            std::process::exit(128 + libc::WTERMSIG(status));
        }
        std::process::exit(libc::WEXITSTATUS(status));
    }
}

#[cfg(target_os = "macos")]
unsafe fn environ_ptr() -> *const *mut libc::c_char {
    unsafe extern "C" {
        static environ: *const *mut libc::c_char;
    }
    unsafe { environ }
}

#[cfg(not(target_os = "macos"))]
pub fn maybe_reexec_disclaimed() {}
