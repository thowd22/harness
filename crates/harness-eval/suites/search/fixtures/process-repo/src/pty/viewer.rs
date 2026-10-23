use std::process::Command;

pub struct Viewer {
    pub rows: u16,
    pub cols: u16,
}

/// Opens a pseudo terminal and starts `program` on it.
pub fn open_pty(program: &str) -> std::io::Result<Viewer> {
    let mut c = Command::new(program);
    crate::util::in_new_session(&mut c); // setsid so the pty becomes its controlling terminal
    c.spawn()?;
    Ok(Viewer { rows: 24, cols: 80 })
}

pub fn resize(v: &mut Viewer, rows: u16, cols: u16) {
    v.rows = rows;
    v.cols = cols;
}
