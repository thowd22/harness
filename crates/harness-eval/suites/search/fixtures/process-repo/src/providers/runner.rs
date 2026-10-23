use std::process::Command;

/// Runs one job in its own session so it can be killed as a group.
pub fn spawn_job(cmd: &str) -> std::io::Result<std::process::Child> {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd);
    // SAFETY-free: setsid is called through the pre_exec helper.
    crate::util::in_new_session(&mut c);
    c.spawn() // setsid: the child leads a new session
}

pub fn job_name(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("job")
}
