// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/policy/mac/renderer.sb

#[expect(
    unsafe_code,
    reason = "macOS Seatbelt sandbox_init C API has no safe std wrapper"
)]
mod ffi {
    use std::ffi::{CStr, CString, c_char, c_int};
    use std::io;
    use std::ptr;

    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    const PROFILE: &str = "\
(version 1)\n\
(deny default)\n\
(allow process-info-pidinfo (target self))\n\
(allow process-info-setcontrol (target self))\n\
(allow mach-lookup (global-name \"com.apple.system.logger\"))\n\
(allow sysctl-read)\n\
(allow file-read-metadata)\n\
(allow file-read-data\n\
  (literal \"/dev/null\")\n\
  (literal \"/dev/urandom\")\n\
  (literal \"/dev/random\")\n\
  (subpath \"/System/Library/Fonts\")\n\
  (subpath \"/Library/Fonts\")\n\
  (subpath \"/System/Library/Frameworks\")\n\
  (subpath \"/System/Library/PrivateFrameworks\")\n\
  (subpath \"/usr/lib\")\n\
  (subpath \"/usr/share/zoneinfo\")\n\
  (literal \"/etc/localtime\"))\n\
(allow file-write-data\n\
  (require-all (literal \"/dev/null\") (vnode-type CHARACTER-DEVICE)))\n";

    pub(super) fn engage() -> io::Result<()> {
        let profile = CString::new(PROFILE).map_err(io::Error::other)?;
        let mut error_buf: *mut c_char = ptr::null_mut();
        // SAFETY: `profile` is a valid NUL-terminated C string and `error_buf` is a stack out-pointer populated by libSystem on error.
        let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error_buf) };
        if rc == 0 {
            return Ok(());
        }
        let message = if error_buf.is_null() {
            "sandbox_init failed".to_owned()
        } else {
            // SAFETY: libSystem allocated `error_buf` as a NUL-terminated string and requires freeing it via `sandbox_free_error`.
            let text = unsafe { CStr::from_ptr(error_buf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error_buf) };
            text
        };
        Err(io::Error::other(message))
    }
}

pub(super) fn engage() -> std::io::Result<()> {
    ffi::engage()
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn sandbox_blocks_filesystem_network_and_exec_in_child() {
        const CHILD_ENV: &str = "TINYBROWSER_MACOS_SANDBOX_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            engage().expect("seatbelt engage");
            let thread_result = std::thread::spawn(|| 42usize)
                .join()
                .expect("worker thread must spawn inside seatbelt");
            assert_eq!(thread_result, 42);
            assert!(std::fs::read("/dev/null").is_ok());
            let fs_err = std::fs::read("/etc/passwd").expect_err("filesystem read must fail");
            assert_eq!(fs_err.kind(), io::ErrorKind::PermissionDenied);
            let write_err = std::fs::write("/tmp/tinybrowser-sandbox-escape-test", b"x")
                .expect_err("filesystem write must fail");
            assert_eq!(write_err.kind(), io::ErrorKind::PermissionDenied);
            let tcp_err =
                std::net::TcpStream::connect("127.0.0.1:1").expect_err("tcp socket must fail");
            assert_eq!(tcp_err.kind(), io::ErrorKind::PermissionDenied);
            let udp_err =
                std::net::UdpSocket::bind("127.0.0.1:0").expect_err("udp socket must fail");
            assert_eq!(udp_err.kind(), io::ErrorKind::PermissionDenied);
            let exec_err = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("true")
                .status()
                .expect_err("exec must fail");
            assert_eq!(exec_err.kind(), io::ErrorKind::PermissionDenied);
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .arg("--exact")
            .arg(
                "child::sandbox::macos::tests::sandbox_blocks_filesystem_network_and_exec_in_child",
            )
            .env(CHILD_ENV, "1")
            .status()
            .expect("spawn sandbox child");
        assert!(
            status.success(),
            "macos sandbox child test failed: {status}"
        );
    }
}
