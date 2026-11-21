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
