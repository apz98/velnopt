//! Entry point for the `velnopt` command.

mod config_file;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "velnopt", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the panel inside a Hyprland session. Send SIGHUP to reload.
    Run {
        /// Path to a Velnopt TOML configuration.
        config: PathBuf,
    },
    /// Validate a configuration without connecting to a compositor.
    Check {
        /// Path to a Velnopt TOML configuration.
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Run { config } => {
            match config_file::load(&config).and_then(|settings| {
                velnopt_wayland::run(config.clone(), settings, || config_file::load(&config))
            }) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{}: {error}", config.display());
                    ExitCode::FAILURE
                }
            }
        }
        Command::Check { config } => match config_file::load(&config) {
            Ok(_) => {
                println!("{}: configuration is valid", config.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{}: {error}", config.display());
                ExitCode::FAILURE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read};

    use crate::config_file::read_config;
    use velnopt_config::MAX_CONFIG_BYTES;

    #[test]
    fn bounds_reads_even_when_input_has_no_length_metadata() {
        let mut stream = io::repeat(b'x').take((MAX_CONFIG_BYTES + 128) as u64);
        let error = read_config(&mut stream).expect_err("unbounded input must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(stream.limit(), 127, "read only the limit plus one byte");
    }

    #[test]
    fn accepts_exact_size_limit() {
        let source = " ".repeat(MAX_CONFIG_BYTES);
        assert_eq!(
            read_config(source.as_bytes()).expect("within limit"),
            source
        );
    }

    #[test]
    fn rejects_non_utf8_input() {
        let error = read_config([0xff].as_slice()).expect_err("TOML must be UTF-8");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn propagates_read_failures() {
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "read denied",
                ))
            }
        }
        let error = read_config(FailingReader).expect_err("read failure must reach caller");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
