use anyhow::Result as AnyHowResult;
use nix::sched::{setns, CloneFlags};
use std::fs::File;

pub fn switch_namespace(pid: i32) -> AnyHowResult<()> {
    let fd = File::open(format!("/proc/{}/ns/net", pid))?;
    setns(fd, CloneFlags::CLONE_NEWNET)?;
    Ok(())
}
