use std::{collections::BTreeMap, path::Path};

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};

use crate::{Error, Result, error::io};

const BPF_RETURN_IMMEDIATE: u16 = 0x06;
// Linux reserves 452 for fchmodat2 across seccompiler's supported ABIs; libc
// currently exposes the name on x86 only.
const FCHMODAT2: i64 = 452;

pub(super) fn forge_sxid_guillotine(path: &Path) -> Result<()> {
    let mut rules = BTreeMap::new();
    for (syscall, mode_argument) in [
        (libc::SYS_fchmod, 1),
        (libc::SYS_fchmodat, 2),
        (FCHMODAT2, 2),
        (libc::SYS_mkdirat, 2),
        (libc::SYS_mknodat, 2),
    ] {
        sever_sxid_modes(&mut rules, syscall, mode_argument)?;
    }
    #[cfg(target_arch = "x86_64")]
    for (syscall, mode_argument) in [
        (libc::SYS_chmod, 1),
        (libc::SYS_mkdir, 1),
        (libc::SYS_mknod, 1),
        (libc::SYS_creat, 1),
    ] {
        sever_sxid_modes(&mut rules, syscall, mode_argument)?;
    }
    sever_sxid_creation(&mut rules, libc::SYS_openat, 2, 3)?;
    #[cfg(target_arch = "x86_64")]
    sever_sxid_creation(&mut rules, libc::SYS_open, 1, 2)?;

    let mut filter = compile_seccomp(rules, SeccompAction::Errno(libc::EPERM as u32))?;

    // RestrictSUIDSGID blocks openat2 because seccomp cannot inspect its
    // indirect mode. Apply that rule after Bubblewrap constructs the sandbox.
    sever_terminal_allow(&mut filter)?;
    filter.append(&mut compile_seccomp(
        BTreeMap::from([(libc::SYS_openat2, Vec::new())]),
        SeccompAction::Errno(libc::ENOSYS as u32),
    )?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| io("create seccomp filter directory", parent, err))?;
    }
    let mut bytes = Vec::with_capacity(filter.len() * 8);
    for instruction in filter {
        bytes.extend_from_slice(&instruction.code.to_ne_bytes());
        bytes.push(instruction.jt);
        bytes.push(instruction.jf);
        bytes.extend_from_slice(&instruction.k.to_ne_bytes());
    }
    std::fs::write(path, bytes).map_err(|err| io("write seccomp filter", path, err))
}

fn sever_sxid_modes(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    syscall: i64,
    mode_argument: u8,
) -> Result<()> {
    for bit in [libc::S_ISUID, libc::S_ISGID] {
        let condition = SeccompCondition::new(
            mode_argument,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::MaskedEq(u64::from(bit)),
            u64::from(bit),
        )
        .map_err(|error| sxid_fault("construct SUID/SGID mode condition", error))?;
        rules.entry(syscall).or_default().push(
            SeccompRule::new(vec![condition])
                .map_err(|error| sxid_fault("construct SUID/SGID mode rule", error))?,
        );
    }
    Ok(())
}

fn sever_sxid_creation(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    syscall: i64,
    flags_argument: u8,
    mode_argument: u8,
) -> Result<()> {
    for bit in [libc::S_ISUID, libc::S_ISGID] {
        let conditions = vec![
            SeccompCondition::new(
                flags_argument,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::MaskedEq(libc::O_CREAT as u64),
                libc::O_CREAT as u64,
            )
            .map_err(|error| sxid_fault("construct creation-flags condition", error))?,
            SeccompCondition::new(
                mode_argument,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::MaskedEq(u64::from(bit)),
                u64::from(bit),
            )
            .map_err(|error| sxid_fault("construct creation-mode condition", error))?,
        ];
        rules.entry(syscall).or_default().push(
            SeccompRule::new(conditions)
                .map_err(|error| sxid_fault("construct SUID/SGID creation rule", error))?,
        );
    }
    Ok(())
}

fn compile_seccomp(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
    match_action: SeccompAction,
) -> Result<BpfProgram> {
    let architecture = std::env::consts::ARCH
        .try_into()
        .map_err(|error| sxid_fault("resolve target architecture", error))?;
    let filter = SeccompFilter::new(rules, SeccompAction::Allow, match_action, architecture)
        .map_err(|error| sxid_fault("construct filter", error))?;
    filter
        .try_into()
        .map_err(|error| sxid_fault("compile filter", error))
}

fn sever_terminal_allow(filter: &mut BpfProgram) -> Result<()> {
    let Some(terminal) = filter.pop() else {
        return Err(sxid_fault("compose filters", "missing terminal action"));
    };
    if terminal.code != BPF_RETURN_IMMEDIATE
        || terminal.jt != 0
        || terminal.jf != 0
        || terminal.k != libc::SECCOMP_RET_ALLOW
    {
        return Err(sxid_fault(
            "compose filters",
            "compiler emitted an unexpected terminal action",
        ));
    }
    Ok(())
}

fn sxid_fault(operation: &'static str, error: impl std::fmt::Display) -> Error {
    Error::Containment {
        layer: "payload seccomp",
        detail: format!("{operation}: {error}"),
    }
}
