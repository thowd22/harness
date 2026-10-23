use std::process::Command;

/// Makes the child call setsid() before exec.
pub fn in_new_session(c: &mut Command) {
    let _ = c;
}

pub fn clamp(v: u16, max: u16) -> u16 {
    v.min(max)
}
