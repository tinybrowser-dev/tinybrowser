// https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md#the-job-object

#[expect(
    unsafe_code,
    reason = "Windows Job Object, token integrity, and mitigation Win32 APIs have no safe std wrapper"
)]
mod ffi {
    use std::ffi::c_void;
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, AllocateAndInitializeSid, FreeSid, PSID,
        SECURITY_MANDATORY_LABEL_AUTHORITY, SID_AND_ATTRIBUTES, SetTokenInformation,
        TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_PRIVILEGES, TOKEN_MANDATORY_LABEL, TokenIntegrityLevel,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
        JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
        JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
        JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::SystemServices::{
        PROCESS_MITIGATION_CHILD_PROCESS_POLICY, PROCESS_MITIGATION_DYNAMIC_CODE_POLICY,
        SE_GROUP_INTEGRITY, SECURITY_MANDATORY_LOW_RID,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcessToken, ProcessChildProcessPolicy, ProcessDynamicCodePolicy,
        SetProcessMitigationPolicy,
    };

    pub(crate) struct ProcessJob(HANDLE);

    // SAFETY: Win32 job object handles are process-global kernel handles safe to move between threads.
    unsafe impl Send for ProcessJob {}

    impl Drop for ProcessJob {
        fn drop(&mut self) {
            // SAFETY: `self.0` is an owned non-null Win32 handle from `CreateJobObjectW`.
            unsafe { CloseHandle(self.0) };
        }
    }

    impl ProcessJob {
        pub(crate) fn assign(process_handle: *mut c_void) -> io::Result<Self> {
            // SAFETY: Null security attributes and name create an anonymous job owned by the browser parent.
            let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let guard = Self(job);

            // SAFETY: Zero-initialized POD Win32 struct with explicit limit flags.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            let ok = unsafe {
                SetInformationJobObject(
                    guard.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }

            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                    | JOB_OBJECT_UILIMIT_EXITWINDOWS
                    | JOB_OBJECT_UILIMIT_GLOBALATOMS
                    | JOB_OBJECT_UILIMIT_HANDLES
                    | JOB_OBJECT_UILIMIT_READCLIPBOARD
                    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
            };
            let ok = unsafe {
                SetInformationJobObject(
                    guard.0,
                    JobObjectBasicUIRestrictions,
                    (&raw const ui).cast(),
                    u32::try_from(size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>()).unwrap_or(0),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }

            // SAFETY: `guard.0` is valid and `process_handle` is the child's live Win32 process handle.
            let ok = unsafe { AssignProcessToJobObject(guard.0, process_handle) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(guard)
        }
    }

    pub(super) fn engage() -> io::Result<()> {
        lower_token_privileges_and_integrity()?;
        enable_process_mitigations()?;
        Ok(())
    }

    fn lower_token_privileges_and_integrity() -> io::Result<()> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: `GetCurrentProcess()` returns the current process pseudo-handle and `token` is a stack out-pointer.
        let ok = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_ADJUST_DEFAULT,
                &mut token,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `token` is valid; `disableallprivileges = 1` strips all privileges from the renderer token.
        let ok = unsafe {
            AdjustTokenPrivileges(token, 1, ptr::null(), 0, ptr::null_mut(), ptr::null_mut())
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `token` was opened above.
            unsafe { CloseHandle(token) };
            return Err(err);
        }

        let mut low_sid: PSID = ptr::null_mut();
        let low_rid = u32::try_from(SECURITY_MANDATORY_LOW_RID).unwrap_or(0x1000);
        // SAFETY: Allocates the well-known S-1-16-4096 Low Mandatory Integrity SID, freed below with `FreeSid`.
        let ok = unsafe {
            AllocateAndInitializeSid(
                &SECURITY_MANDATORY_LABEL_AUTHORITY,
                1,
                low_rid,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                &mut low_sid,
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `token` was opened above.
            unsafe { CloseHandle(token) };
            return Err(err);
        }

        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: low_sid,
                Attributes: u32::try_from(SE_GROUP_INTEGRITY).unwrap_or(0x20),
            },
        };
        // SAFETY: `token` and `low_sid` are valid for the duration of `SetTokenInformation`.
        let ok = unsafe {
            SetTokenInformation(
                token,
                TokenIntegrityLevel,
                (&raw const label).cast(),
                u32::try_from(size_of::<TOKEN_MANDATORY_LABEL>()).unwrap_or(0),
            )
        };
        // SAFETY: `low_sid` was allocated by `AllocateAndInitializeSid` and `token` by `OpenProcessToken`.
        unsafe {
            FreeSid(low_sid);
            CloseHandle(token);
        }
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn enable_process_mitigations() -> io::Result<()> {
        // SAFETY: Zero-initialized POD mitigation struct; bit 0 sets `NoChildProcessCreation`.
        let mut child_policy: PROCESS_MITIGATION_CHILD_PROCESS_POLICY = unsafe { zeroed() };
        child_policy.Anonymous.Flags = 1;
        // SAFETY: `child_policy` matches `ProcessChildProcessPolicy` size and layout.
        let ok = unsafe {
            SetProcessMitigationPolicy(
                ProcessChildProcessPolicy,
                (&raw const child_policy).cast(),
                size_of::<PROCESS_MITIGATION_CHILD_PROCESS_POLICY>(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: Zero-initialized POD mitigation struct; bit 0 sets `ProhibitDynamicCode`.
        let mut dynamic_policy: PROCESS_MITIGATION_DYNAMIC_CODE_POLICY = unsafe { zeroed() };
        dynamic_policy.Anonymous.Flags = 1;
        // SAFETY: `dynamic_policy` matches `ProcessDynamicCodePolicy` size and layout.
        let ok = unsafe {
            SetProcessMitigationPolicy(
                ProcessDynamicCodePolicy,
                (&raw const dynamic_policy).cast(),
                size_of::<PROCESS_MITIGATION_DYNAMIC_CODE_POLICY>(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub(crate) use ffi::ProcessJob;

pub(super) fn engage() -> std::io::Result<()> {
    ffi::engage()
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;

    use super::*;

    #[test]
    fn sandbox_blocks_child_process_creation_in_child() {
        const CHILD_ENV: &str = "TINYBROWSER_WINDOWS_SANDBOX_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            engage().expect("windows sandbox engage");
            let thread_result = std::thread::spawn(|| 42usize)
                .join()
                .expect("worker thread must spawn inside windows sandbox");
            assert_eq!(thread_result, 42);
            let spawn_err = std::process::Command::new("cmd.exe")
                .args(["/C", "exit", "0"])
                .status()
                .expect_err("child process creation must fail");
            assert_eq!(spawn_err.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }
        let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .arg("--exact")
            .arg("child::sandbox::windows::tests::sandbox_blocks_child_process_creation_in_child")
            .env(CHILD_ENV, "1")
            .spawn()
            .expect("spawn sandbox child");
        let _job = ProcessJob::assign(child.as_raw_handle()).expect("assign child to parent job");
        let status = child.wait().expect("wait sandbox child");
        assert!(
            status.success(),
            "windows sandbox child test failed: {status}"
        );
    }
}
