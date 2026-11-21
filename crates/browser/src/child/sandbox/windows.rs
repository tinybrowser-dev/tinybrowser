// https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md#the-job-object

#[expect(
    unsafe_code,
    reason = "Windows Job Object Win32 API has no safe std wrapper"
)]
mod ffi {
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::ptr;

    use windows_sys::Win32::Foundation::CloseHandle;
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
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn engage() -> io::Result<()> {
        // SAFETY: Passing null security attributes and null name creates an anonymous job object owned by this process.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: Zero-initialized POD Win32 struct with explicit limit flags.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;

        let ok = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `job` is a valid handle created above.
            unsafe { CloseHandle(job) };
            return Err(err);
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
                job,
                JobObjectBasicUIRestrictions,
                (&raw const ui).cast(),
                u32::try_from(size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>()).unwrap_or(0),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `job` is a valid handle created above.
            unsafe { CloseHandle(job) };
            return Err(err);
        }

        // SAFETY: `job` is valid and `GetCurrentProcess()` returns the pseudo-handle for this process; the handle stays open for process lifetime.
        let ok = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `job` is a valid handle created above.
            unsafe { CloseHandle(job) };
            return Err(err);
        }
        Ok(())
    }
}

pub(super) fn engage() -> std::io::Result<()> {
    ffi::engage()
}
