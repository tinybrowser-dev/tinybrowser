// https://docs.kernel.org/userspace-api/landlock.html
// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/policy/linux/bpf_renderer_policy_linux.cc

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

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
const CLONE_NAMESPACE_FLAGS: [u64; 8] = [
    0x0002_0000,
    0x0200_0000,
    0x0400_0000,
    0x0800_0000,
    0x1000_0000,
    0x2000_0000,
    0x4000_0000,
    0x0000_0080,
];

pub(super) fn engage() -> io::Result<()> {
    engage_landlock()?;
    engage_seccomp()?;
    Ok(())
}

fn engage_landlock() -> io::Result<()> {
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
    for path in allowed_read_paths() {
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

fn allowed_read_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/etc/fonts"),
        PathBuf::from("/etc/localtime"),
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        PathBuf::from("/usr/share/zoneinfo"),
        PathBuf::from("/run/current-system/sw/share/X11/fonts"),
        PathBuf::from("/run/current-system/sw/share/fonts"),
        PathBuf::from("/nix/store"),
        PathBuf::from("/system/fonts"),
        PathBuf::from("/system/etc/fonts.xml"),
        PathBuf::from("/dev/null"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/zero"),
        PathBuf::from("/proc/self/stat"),
        PathBuf::from("/proc/self/maps"),
    ];
    if let Ok(file) = std::env::var("FONTCONFIG_FILE") {
        let path = PathBuf::from(&file);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            paths.push(parent.to_path_buf());
        }
        paths.push(path);
    }
    if let Ok(list) = std::env::var("FONTCONFIG_PATH") {
        for entry in std::env::split_paths(&list) {
            if !entry.as_os_str().is_empty() {
                paths.push(entry);
            }
        }
    }
    if let Ok(prefix) = std::env::var("PREFIX") {
        let base = Path::new(&prefix);
        paths.push(base.join("etc/fonts"));
        paths.push(base.join("share/fonts"));
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

fn build_seccomp_program(arch: TargetArch) -> io::Result<BpfProgram> {
    let rules = seccomp_rule_map()?;
    let eperm = u32::try_from(libc::EPERM).unwrap_or(1);
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(eperm),
        arch,
    )
    .map_err(io::Error::other)?;
    filter.try_into().map_err(io::Error::other)
}

fn seccomp_rule_map() -> io::Result<BTreeMap<i64, Vec<SeccompRule>>> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for syscall in denied_syscalls() {
        rules.insert(syscall, Vec::new());
        #[cfg(target_arch = "x86_64")]
        rules.insert(syscall | X32_SYSCALL_BIT, Vec::new());
    }
    #[cfg(target_arch = "x86_64")]
    for x32_nr in 512..=547 {
        rules.insert(x32_nr | X32_SYSCALL_BIT, Vec::new());
    }
    let clone_deny_rules = clone_deny_rules()?;
    rules.insert(libc::SYS_clone, clone_deny_rules);
    #[cfg(target_arch = "x86_64")]
    rules.insert(libc::SYS_clone | X32_SYSCALL_BIT, Vec::new());
    Ok(rules)
}

fn clone_deny_rules() -> io::Result<Vec<SeccompRule>> {
    let mut rules = Vec::with_capacity(1 + CLONE_NAMESPACE_FLAGS.len());
    let not_thread = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::MaskedEq(CLONE_THREAD),
        0,
    )
    .map_err(io::Error::other)?;
    rules.push(SeccompRule::new(vec![not_thread]).map_err(io::Error::other)?);
    for flag in CLONE_NAMESPACE_FLAGS {
        let has_ns = SeccompCondition::new(
            0,
            SeccompCmpArgLen::Qword,
            SeccompCmpOp::MaskedEq(flag),
            flag,
        )
        .map_err(io::Error::other)?;
        rules.push(SeccompRule::new(vec![has_ns]).map_err(io::Error::other)?);
    }
    Ok(rules)
}

fn denied_syscalls() -> Vec<i64> {
    vec![
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_getfd,
        libc::SYS_kcmp,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_chroot,
        libc::SYS_pivot_root,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        libc::SYS_bpf,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_userfaultfd,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_perf_event_open,
        libc::SYS_kexec_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_reboot,
        libc::SYS_syslog,
        libc::SYS_quotactl,
        libc::SYS_acct,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_personality,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_fork,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_vfork,
    ]
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

        let rules = seccomp_rule_map().expect("rule map");
        assert!(rules.contains_key(&libc::SYS_pidfd_open));
        assert!(rules.contains_key(&libc::SYS_pidfd_getfd));
        assert!(rules.contains_key(&libc::SYS_kcmp));
        assert_eq!(
            rules.get(&libc::SYS_clone).map(Vec::len),
            Some(1 + CLONE_NAMESPACE_FLAGS.len())
        );
        #[cfg(target_arch = "x86_64")]
        {
            assert!(rules.contains_key(&(libc::SYS_socket | X32_SYSCALL_BIT)));
            assert!(rules.contains_key(&(libc::SYS_clone | X32_SYSCALL_BIT)));
            assert!(rules.contains_key(&(0x0208 | X32_SYSCALL_BIT)));
        }
    }

    #[test]
    fn sandbox_blocks_filesystem_network_and_exec_in_child() {
        const CHILD_ENV: &str = "TINYBROWSER_SANDBOX_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            engage().expect("sandbox engage");
            let thread_result = std::thread::spawn(|| 42usize)
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
