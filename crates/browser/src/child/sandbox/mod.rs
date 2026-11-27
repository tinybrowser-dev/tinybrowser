// https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(windows)]
pub(crate) use windows::ProcessJob;

use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererSandbox {
    #[default]
    Enabled,
    Disabled,
}

pub(super) fn engage(font_dirs: &[PathBuf]) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        linux::engage(font_dirs)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = font_dirs;
        macos::engage()
    }
    #[cfg(target_os = "windows")]
    {
        let _ = font_dirs;
        windows::engage()
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        let _ = font_dirs;
        Ok(())
    }
}
