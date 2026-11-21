// https://docs.kernel.org/userspace-api/landlock.html
// https://raw.githubusercontent.com/chromium/chromium/main/sandbox/policy/linux/bpf_renderer_policy_linux.cc

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};

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
        logging::debug!(
            target: "renderer::sandbox",
            "landlock not enforced by host kernel"
        );
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
        logging::debug!(
            target: "renderer::sandbox",
            "seccomp unsupported on arch {}",
            std::env::consts::ARCH
        );
        return Ok(());
    };
    let program = build_seccomp_program(arch)?;
    match seccompiler::apply_filter_all_threads(&program) {
        Ok(()) => Ok(()),
        Err(seccompiler::Error::Seccomp(error)) if error.raw_os_error() == Some(libc::ENOSYS) => {
            logging::debug!(
                target: "renderer::sandbox",
                "seccomp not supported by host kernel"
            );
            Ok(())
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

fn build_seccomp_program(arch: TargetArch) -> io::Result<BpfProgram> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for syscall in denied_syscalls() {
        rules.insert(syscall, Vec::new());
    }
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

fn denied_syscalls() -> Vec<i64> {
    let mut list = Vec::from([
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
    ]);
    #[cfg(target_arch = "x86_64")]
    {
        list.push(libc::SYS_fork);
        list.push(libc::SYS_vfork);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seccomp_filter_compiles_for_host_arch() {
        let arch = TargetArch::try_from(std::env::consts::ARCH).expect("host arch");
        let program = build_seccomp_program(arch).expect("bpf program");
        assert_ne!(program.len(), 0);
    }

    #[test]
    fn sandbox_blocks_filesystem_network_and_exec_in_child() {
        const CHILD_ENV: &str = "TINYBROWSER_SANDBOX_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            engage().expect("sandbox engage");
            let fs_err = std::fs::read("/etc/passwd").expect_err("filesystem read must fail");
            assert_eq!(fs_err.kind(), io::ErrorKind::PermissionDenied);
            let net_err =
                std::net::TcpStream::connect("127.0.0.1:1").expect_err("socket must fail");
            assert_eq!(net_err.kind(), io::ErrorKind::PermissionDenied);
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
