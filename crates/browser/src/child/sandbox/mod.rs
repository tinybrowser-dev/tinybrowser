// https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::io;

pub(super) fn engage() -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        linux::engage()
    }
    #[cfg(target_os = "macos")]
    {
        macos::engage()
    }
    #[cfg(target_os = "windows")]
    {
        windows::engage()
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        Ok(())
    }
}
