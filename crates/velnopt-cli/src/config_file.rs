//! Bounded, nonblocking config file reads, shared by startup, `check`, and reload.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use rustix::fs::{Mode, OFlags, open};
use velnopt_config::{Config, MAX_CONFIG_BYTES};

pub(crate) fn load(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err("configuration must be a regular file".into());
    }
    Ok(velnopt_config::parse(&read_config(file)?)?)
}

pub(crate) fn read_config(reader: impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            velnopt_config::ConfigError::TooLarge,
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
