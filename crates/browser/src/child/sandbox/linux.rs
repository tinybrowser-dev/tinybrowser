// https://docs.kernel.org/userspace-api/landlock.html
// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/linux/seccomp-bpf-helpers/baseline_policy.cc
// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/policy/linux/bpf_renderer_policy_linux.cc

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: i64 = 0x4000_0000;

const CLONE_THREAD: u64 = 0x0001_0000;
const CLONE_NAMESPACE_MASK: u64 = 0x0002_0000
    | 0x0200_0000
    | 0x0400_0000
    | 0x0800_0000
    | 0x1000_0000
    | 0x2000_0000
    | 0x4000_0000
    | 0x0000_0080;

// https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/asm-generic/unistd.h — libc's Android bindings omit these arm64 generic numbers; the kernel ABI is shared.
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
mod sysno {
    pub(super) const SYS_FSTAT: i64 = 80;
    pub(super) const SYS_FSTATFS: i64 = 44;
    pub(super) const SYS_NEWFSTATAT: i64 = 79;
    pub(super) const SYS_RSEQ: i64 = 293;
}

#[cfg(not(all(target_os = "android", target_arch = "aarch64")))]
mod sysno {
    pub(super) use libc::{
        SYS_fstat as SYS_FSTAT, SYS_fstatfs as SYS_FSTATFS, SYS_newfstatat as SYS_NEWFSTATAT,
        SYS_rseq as SYS_RSEQ,
    };
}

pub(super) fn engage(font_dirs: &[PathBuf]) -> io::Result<()> {
    engage_landlock(font_dirs)?;
    engage_seccomp()?;
    Ok(())
}

fn engage_landlock(font_dirs: &[PathBuf]) -> io::Result<()> {
    let abi = ABI::V5;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(abi))
        .map_err(io::Error::other)?
        .handle_access(AccessNet::from_all(abi))
        .map_err(io::Error::other)?
        .create()
        .map_err(io::Error::other)?;

    let read_access = AccessFs::from_read(abi);
    for path in allowed_read_paths(font_dirs) {
        let Ok(fd) = PathFd::new(&path) else {
            continue;
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, read_access))
            .map_err(io::Error::other)?;
    }

    let status = ruleset.restrict_self().map_err(io::Error::other)?;
    if matches!(status.ruleset, RulesetStatus::NotEnforced) {
        #[cfg(target_os = "android")]
        {
            logging::debug!(
                target: "renderer::sandbox",
                "landlock not enforced by host kernel"
            );
        }
        #[cfg(not(target_os = "android"))]
        {
            return Err(io::Error::other(
                "renderer Landlock sandbox not enforced by host kernel (pass --no-sandbox only inside an already-isolated container)",
            ));
        }
    }
    Ok(())
}

fn allowed_read_paths(font_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/etc/localtime"),
        PathBuf::from("/usr/share/zoneinfo"),
        PathBuf::from("/nix/store"),
        PathBuf::from("/dev/null"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/zero"),
        PathBuf::from("/proc/self/stat"),
        PathBuf::from("/proc/self/maps"),
    ];
    for dir in font_dirs {
        if !paths.contains(dir) {
            paths.push(dir.clone());
        }
    }
    paths
}

fn engage_seccomp() -> io::Result<()> {
    let Ok(arch) = TargetArch::try_from(std::env::consts::ARCH) else {
        #[cfg(target_os = "android")]
        {
            logging::debug!(
                target: "renderer::sandbox",
                "seccomp unsupported on arch {}",
                std::env::consts::ARCH
            );
            return Ok(());
        }
        #[cfg(not(target_os = "android"))]
        {
            return Err(io::Error::other(format!(
                "seccomp unsupported on arch {}",
                std::env::consts::ARCH
            )));
        }
    };
    let clone3_program = build_clone3_fallback_program(arch)?;
    let main_program = build_seccomp_program(arch)?;
    for program in [&clone3_program, &main_program] {
        match seccompiler::apply_filter_all_threads(program) {
            Ok(()) => {}
            #[cfg(target_os = "android")]
            Err(seccompiler::Error::Seccomp(error))
                if error.raw_os_error() == Some(libc::ENOSYS) =>
            {
                logging::debug!(
                    target: "renderer::sandbox",
                    "seccomp not supported by host kernel"
                );
                return Ok(());
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    Ok(())
}

// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/linux/seccomp-bpf-helpers/baseline_policy.cc
fn build_clone3_fallback_program(arch: TargetArch) -> io::Result<BpfProgram> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    rules.insert(libc::SYS_clone3, Vec::new());
    #[cfg(target_arch = "x86_64")]
    rules.insert(libc::SYS_clone3 | X32_SYSCALL_BIT, Vec::new());
    let enosys = u32::try_from(libc::ENOSYS).unwrap_or(38);
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(enosys),
        arch,
    )
    .map_err(io::Error::other)?;
    filter.try_into().map_err(io::Error::other)
}

// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/policy/linux/bpf_renderer_policy_linux.cc
fn build_seccomp_program(arch: TargetArch) -> io::Result<BpfProgram> {
    let rules = seccomp_allow_map()?;
    let eperm = u32::try_from(libc::EPERM).unwrap_or(1);
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(eperm),
        SeccompAction::Allow,
        arch,
    )
    .map_err(io::Error::other)?;
    filter.try_into().map_err(io::Error::other)
}

fn seccomp_allow_map() -> io::Result<BTreeMap<i64, Vec<SeccompRule>>> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for syscall in allowed_syscalls() {
        rules.insert(syscall, Vec::new());
    }
    rules.insert(libc::SYS_clone3, Vec::new());
    rules.insert(libc::SYS_clone, clone_allow_rules()?);
    rules.insert(libc::SYS_prctl, prctl_allow_rules()?);
    Ok(rules)
}

fn clone_allow_rules() -> io::Result<Vec<SeccompRule>> {
    let is_thread = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::MaskedEq(CLONE_THREAD),
        CLONE_THREAD,
    )
    .map_err(io::Error::other)?;
    let no_namespaces = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::MaskedEq(CLONE_NAMESPACE_MASK),
        0,
    )
    .map_err(io::Error::other)?;
    Ok(vec![
        SeccompRule::new(vec![is_thread, no_namespaces]).map_err(io::Error::other)?,
    ])
}

fn prctl_allow_rules() -> io::Result<Vec<SeccompRule>> {
    let mut rules = Vec::with_capacity(2);
    for option in [libc::PR_SET_NAME, libc::PR_GET_NAME] {
        let cond = SeccompCondition::new(
            0,
            SeccompCmpArgLen::Qword,
            SeccompCmpOp::Eq,
            u64::try_from(option).unwrap_or(0),
        )
        .map_err(io::Error::other)?;
        rules.push(SeccompRule::new(vec![cond]).map_err(io::Error::other)?);
    }
    Ok(rules)
}

const ALLOWED_SYSCALLS: &[i64] = &[
    libc::SYS_brk,
    libc::SYS_mmap,
    libc::SYS_munmap,
    libc::SYS_mprotect,
    libc::SYS_madvise,
    libc::SYS_mremap,
    libc::SYS_mlock,
    libc::SYS_munlock,
    libc::SYS_futex,
    libc::SYS_set_robust_list,
    libc::SYS_get_robust_list,
    sysno::SYS_RSEQ,
    libc::SYS_sched_yield,
    libc::SYS_sched_getaffinity,
    libc::SYS_sched_getparam,
    libc::SYS_sched_getscheduler,
    libc::SYS_sched_setscheduler,
    libc::SYS_gettid,
    libc::SYS_getpid,
    libc::SYS_getppid,
    libc::SYS_getuid,
    libc::SYS_geteuid,
    libc::SYS_getgid,
    libc::SYS_getegid,
    libc::SYS_set_tid_address,
    libc::SYS_prlimit64,
    libc::SYS_getrusage,
    libc::SYS_membarrier,
    libc::SYS_tgkill,
    libc::SYS_tkill,
    libc::SYS_exit,
    libc::SYS_exit_group,
    libc::SYS_clock_gettime,
    libc::SYS_clock_getres,
    libc::SYS_clock_nanosleep,
    libc::SYS_nanosleep,
    libc::SYS_gettimeofday,
    libc::SYS_getrandom,
    libc::SYS_times,
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    libc::SYS_rt_sigreturn,
    libc::SYS_sigaltstack,
    libc::SYS_restart_syscall,
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_readv,
    libc::SYS_writev,
    libc::SYS_pread64,
    libc::SYS_pwrite64,
    libc::SYS_close,
    libc::SYS_close_range,
    libc::SYS_lseek,
    sysno::SYS_FSTAT,
    sysno::SYS_NEWFSTATAT,
    libc::SYS_statx,
    sysno::SYS_FSTATFS,
    libc::SYS_getdents64,
    libc::SYS_openat,
    libc::SYS_readlinkat,
    libc::SYS_faccessat,
    libc::SYS_faccessat2,
    libc::SYS_fcntl,
    libc::SYS_dup,
    libc::SYS_dup3,
    libc::SYS_pipe2,
    libc::SYS_eventfd2,
    libc::SYS_epoll_create1,
    libc::SYS_epoll_ctl,
    libc::SYS_epoll_pwait,
    libc::SYS_epoll_pwait2,
    libc::SYS_ppoll,
    libc::SYS_pselect6,
    libc::SYS_ioctl,
    libc::SYS_recvfrom,
    libc::SYS_recvmsg,
    libc::SYS_sendto,
    libc::SYS_sendmsg,
    libc::SYS_shutdown,
    libc::SYS_getsockopt,
    libc::SYS_setsockopt,
    libc::SYS_getsockname,
    libc::SYS_getpeername,
    libc::SYS_fsync,
    libc::SYS_fdatasync,
    libc::SYS_getcwd,
    libc::SYS_uname,
    libc::SYS_sysinfo,
];

#[cfg(target_arch = "x86_64")]
const X86_64_ALLOWED_SYSCALLS: &[i64] = &[
    libc::SYS_open,
    libc::SYS_stat,
    libc::SYS_lstat,
    libc::SYS_access,
    libc::SYS_readlink,
    libc::SYS_dup2,
    libc::SYS_pipe,
    libc::SYS_poll,
    libc::SYS_select,
    libc::SYS_epoll_create,
    libc::SYS_epoll_wait,
    libc::SYS_eventfd,
    libc::SYS_arch_prctl,
    libc::SYS_time,
    libc::SYS_getdents,
    libc::SYS_getrlimit,
];

fn allowed_syscalls() -> Vec<i64> {
    let mut syscalls = ALLOWED_SYSCALLS.to_vec();
    #[cfg(target_arch = "x86_64")]
    syscalls.extend_from_slice(X86_64_ALLOWED_SYSCALLS);
    syscalls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seccomp_filter_compiles_for_host_arch() {
        let arch = TargetArch::try_from(std::env::consts::ARCH).expect("host arch");
        let clone3 = build_clone3_fallback_program(arch).expect("clone3 bpf");
        let program = build_seccomp_program(arch).expect("bpf program");
        assert_ne!(clone3.len(), 0);
        assert_ne!(program.len(), 0);

        let rules = seccomp_allow_map().expect("allow map");
        assert!(rules.contains_key(&libc::SYS_read));
        assert!(rules.contains_key(&libc::SYS_futex));
        assert_eq!(rules.get(&libc::SYS_clone).map(Vec::len), Some(1));
        assert_eq!(rules.get(&libc::SYS_prctl).map(Vec::len), Some(2));
        for forbidden in [
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_execve,
            libc::SYS_execveat,
            libc::SYS_ptrace,
            libc::SYS_pidfd_open,
            libc::SYS_pidfd_getfd,
            libc::SYS_kcmp,
            libc::SYS_io_uring_setup,
            libc::SYS_unshare,
            libc::SYS_mount,
            libc::SYS_bpf,
        ] {
            assert!(
                !rules.contains_key(&forbidden),
                "syscall {forbidden} must not be in the seccomp allowlist"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            assert!(!rules.contains_key(&(libc::SYS_read | X32_SYSCALL_BIT)));
            assert!(!rules.contains_key(&(libc::SYS_clone | X32_SYSCALL_BIT)));
            assert!(!rules.contains_key(&(0x0208 | X32_SYSCALL_BIT)));
        }
    }

    #[test]
    fn sandbox_blocks_filesystem_network_and_exec_in_child() {
        const CHILD_ENV: &str = "TINYBROWSER_SANDBOX_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            let font_dirs = renderer::preload_system_fonts();
            let had_fonts = renderer::Fonts::default().has_families();
            engage(&font_dirs).expect("sandbox engage");
            if had_fonts {
                assert!(
                    renderer::Fonts::default().has_families(),
                    "preloaded system fonts must resolve inside the sandbox"
                );
            }
            let thread_result = std::thread::Builder::new()
                .name("sandbox-worker".to_owned())
                .spawn(|| 42usize)
                .expect("spawn named worker")
                .join()
                .expect("worker thread must still spawn under clone3->clone fallback");
            assert_eq!(thread_result, 42);
            let fs_err = std::fs::read("/etc/passwd").expect_err("filesystem read must fail");
            assert_eq!(fs_err.kind(), io::ErrorKind::PermissionDenied);
            let write_err = std::fs::write("/tmp/tinybrowser-sandbox-escape-test", b"x")
                .expect_err("filesystem write must fail");
            assert_eq!(write_err.kind(), io::ErrorKind::PermissionDenied);
            let proc_err =
                std::fs::read("/proc/self/status").expect_err("unlisted proc read must fail");
            assert_eq!(proc_err.kind(), io::ErrorKind::PermissionDenied);
            let tcp_err =
                std::net::TcpStream::connect("127.0.0.1:1").expect_err("tcp socket must fail");
            assert_eq!(tcp_err.kind(), io::ErrorKind::PermissionDenied);
            let udp_err =
                std::net::UdpSocket::bind("127.0.0.1:0").expect_err("udp socket must fail");
            assert_eq!(udp_err.kind(), io::ErrorKind::PermissionDenied);
            let unix_err =
                std::os::unix::net::UnixStream::pair().expect_err("unix socketpair must fail");
            assert_eq!(unix_err.kind(), io::ErrorKind::PermissionDenied);
            let exec_err = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("true")
                .status()
                .expect_err("execve must fail");
            assert_eq!(exec_err.kind(), io::ErrorKind::PermissionDenied);
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .arg("--exact")
            .arg(
                "child::sandbox::linux::tests::sandbox_blocks_filesystem_network_and_exec_in_child",
            )
            .env(CHILD_ENV, "1")
            .status()
            .expect("spawn sandbox child");
        assert!(status.success(), "sandbox child test failed: {status}");
    }
}
