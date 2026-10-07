use std::{env, io, path::PathBuf};

pub fn default_data_dir() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("ricoh-monitor"));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config/ricoh-monitor"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "未设置 XDG_CONFIG_HOME 或 HOME"))
}

pub fn service_unit() -> &'static str {
    include_str!("../systemd/ricoh-monitor.service")
}
