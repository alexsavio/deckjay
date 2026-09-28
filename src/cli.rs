//! The command line. `deckjay [CONFIG]` runs the player; every other command
//! does one job and exits. The old spellings `--check`, `--preview FILE` and
//! `--blank` still work: systemd's `ExecStopPost` runs `deckjay --blank`.
//!
//! Exit codes: 0 all good; 1 an error (and for `check`: a problem); 2 for
//! `check` when it found warnings only; 64 for a command line clap cannot
//! read, so a typo never looks like a `check` result.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand};

use crate::config;
use crate::simulator::Model;

const DEFAULT_CONFIG: &str = "config.toml";

/// `EX_USAGE` from `sysexits.h`.
const EXIT_USAGE: i32 = 64;

const AFTER_HELP: &str = "\
Exit codes of `check`: 0 all good, 1 a problem (a feed that cannot be read,
a source with nothing to play, a speaker that does not answer), 2 warnings
only (an item with no cover, no Stream Deck plugged in, no spotify.device,
a podcast source before its first refresh). A wrong command line exits
with 64.";

/// Reads the command line, or prints help, the version or the error and
/// exits.
pub fn parse() -> Command {
    match Cli::try_parse() {
        Ok(cli) => cli.into_command(),
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            err.exit()
        }
        Err(err) => {
            // The message goes to stderr; a failed print cannot be reported.
            let _ = err.print();
            std::process::exit(EXIT_USAGE)
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    version,
    about = "A music player with no screen: pictures on a Stream Deck, one press plays",
    after_help = AFTER_HELP,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,

    /// Same as `deckjay check`
    #[arg(long, hide = true)]
    check: bool,

    /// Same as `deckjay preview FILE`; like the command, it takes no
    /// simulator and no advertise host.
    #[arg(
        long,
        hide = true,
        value_name = "FILE.png",
        conflicts_with_all = ["simulator", "advertise_host"]
    )]
    preview: Option<PathBuf>,

    /// Same as `deckjay blank`, which takes nothing else.
    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["config", "simulator", "advertise_host", "check", "preview"]
    )]
    blank: bool,
}

impl Cli {
    /// The old options become the commands they stand for; `--preview` wins
    /// over `--check`, as it did.
    fn into_command(self) -> Command {
        if let Some(command) = self.command {
            return command;
        }
        if self.blank {
            return Command::Blank;
        }
        if let Some(file) = self.preview {
            return Command::Preview {
                file,
                config: self.run.config,
            };
        }
        if self.check {
            return Command::Check(self.run);
        }
        Command::Run(self.run)
    }
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum Command {
    /// Run the player; the default when no command is given
    Run(RunArgs),

    /// List the sources and their items, the Stream Decks and the speaker, then exit
    Check(RunArgs),

    /// Read and check CONFIG, then exit; opens no deck and uses no network
    CheckConfig {
        /// Path to config.toml
        #[arg(default_value = DEFAULT_CONFIG, value_name = "CONFIG")]
        config: PathBuf,
    },

    /// Draw the 15-key layout into a picture, then exit
    Preview {
        /// The picture to write
        #[arg(value_name = "FILE.png")]
        file: PathBuf,

        /// Path to config.toml
        #[arg(default_value = DEFAULT_CONFIG, value_name = "CONFIG")]
        config: PathBuf,
    },

    /// Turn the USB Stream Deck dark, then exit; deckjay does this itself when it is stopped
    Blank,

    /// Run the web Stream Deck simulator
    Simulator {
        /// mk2, mini, neo, xl or plus
        #[arg(long, default_value = "mk2", value_name = "NAME", value_parser = parse_model)]
        model: Model,

        /// Port of the simulator page and API
        #[arg(long, default_value_t = 8090)]
        port: u16,
    },

    #[command(about = "Sign in to Spotify once; saves the login in state_dir")]
    SpotifyLogin {
        /// Path to config.toml
        #[arg(default_value = DEFAULT_CONFIG, value_name = "CONFIG")]
        config: PathBuf,

        /// Where the browser comes back to
        #[arg(long, default_value = "127.0.0.1:8898", value_name = "ADDR")]
        listen: SocketAddr,
    },
}

/// What `run` and `check` share.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct RunArgs {
    /// Path to config.toml
    #[arg(default_value = DEFAULT_CONFIG, value_name = "CONFIG")]
    pub config: PathBuf,

    #[arg(
        long,
        value_name = "URL",
        help = "Use the deck simulator at URL, e.g. http://localhost:8090, instead of a USB Stream Deck"
    )]
    pub simulator: Option<String>,

    #[arg(
        long,
        value_name = "HOST",
        value_parser = parse_advertise_host,
        help = "The address the speaker uses to reach this program; overrides advertise_host in the config"
    )]
    pub advertise_host: Option<String>,
}

fn parse_model(name: &str) -> Result<Model, String> {
    Model::parse(name).ok_or_else(|| format!("use one of: {}", Model::NAMES.join(", ")))
}

/// `compose.sim.yaml` passes `${HOST_IP:-}`, so an empty value means the
/// recipe that finds the address was skipped.
fn parse_advertise_host(host: &str) -> Result<String, String> {
    if host.is_empty() {
        return Err("it is empty; in Docker, start with `just sim`".into());
    }
    config::check_advertise_host(host).map_err(|err| format!("{err:#}"))?;
    Ok(host.to_string())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Command {
        Cli::try_parse_from(std::iter::once("deckjay").chain(args.iter().copied()))
            .unwrap()
            .into_command()
    }

    fn error(args: &[&str]) -> clap::Error {
        Cli::try_parse_from(std::iter::once("deckjay").chain(args.iter().copied())).unwrap_err()
    }

    fn run(config: &str) -> RunArgs {
        RunArgs {
            config: config.into(),
            simulator: None,
            advertise_host: None,
        }
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_runs_the_player_with_the_default_config() {
        assert_eq!(parse(&[]), Command::Run(run("config.toml")));
    }

    #[test]
    fn a_path_is_the_config() {
        assert_eq!(
            parse(&["/etc/deckjay/config.toml"]),
            Command::Run(run("/etc/deckjay/config.toml"))
        );
        assert_eq!(
            parse(&["run", "other.toml"]),
            Command::Run(run("other.toml"))
        );
    }

    #[test]
    fn run_takes_the_simulator_and_the_advertise_host() {
        let Command::Run(args) = parse(&[
            "c.toml",
            "--simulator",
            "http://deck-sim:8090",
            "--advertise-host",
            "192.168.1.9",
        ]) else {
            panic!("not a run");
        };
        assert_eq!(args.config, PathBuf::from("c.toml"));
        assert_eq!(args.simulator.as_deref(), Some("http://deck-sim:8090"));
        assert_eq!(args.advertise_host.as_deref(), Some("192.168.1.9"));
    }

    #[test]
    fn an_empty_or_bad_advertise_host_is_an_error() {
        let err = error(&["--advertise-host", ""]).to_string();
        assert!(err.contains("just sim"), "{err}");
        let err = error(&["--advertise-host", "http://x"]).to_string();
        assert!(err.contains("without scheme"), "{err}");
    }

    #[test]
    fn old_options_are_the_commands() {
        assert_eq!(parse(&["--blank"]), Command::Blank);
        assert_eq!(parse(&["--check", "c.toml"]), Command::Check(run("c.toml")));
        assert_eq!(
            parse(&["--preview", "out.png", "c.toml"]),
            Command::Preview {
                file: "out.png".into(),
                config: "c.toml".into()
            }
        );
        // As before: --preview wins.
        assert!(matches!(
            parse(&["--check", "--preview", "out.png"]),
            Command::Preview { .. }
        ));
    }

    #[test]
    fn commands_take_their_arguments() {
        let Command::Check(args) = parse(&["check", "c.toml", "--simulator", "http://s:1"]) else {
            panic!("not a check");
        };
        assert_eq!(args.simulator.as_deref(), Some("http://s:1"));
        assert_eq!(
            parse(&["check-config", "/etc/deckjay/config.toml"]),
            Command::CheckConfig {
                config: "/etc/deckjay/config.toml".into()
            }
        );
        assert_eq!(
            parse(&["preview", "layout.png"]),
            Command::Preview {
                file: "layout.png".into(),
                config: "config.toml".into()
            }
        );
        assert_eq!(parse(&["blank"]), Command::Blank);
        assert_eq!(
            parse(&["simulator", "--model", "XL", "--port", "9000"]),
            Command::Simulator {
                model: Model::Xl,
                port: 9000
            }
        );
        assert_eq!(
            parse(&["simulator"]),
            Command::Simulator {
                model: Model::Mk2,
                port: 8090
            }
        );
        assert_eq!(
            parse(&["spotify-login", "--listen", "0.0.0.0:9"]),
            Command::SpotifyLogin {
                config: "config.toml".into(),
                listen: "0.0.0.0:9".parse().unwrap()
            }
        );
    }

    #[test]
    fn old_options_take_no_more_than_the_commands_do() {
        for args in [
            &["--preview", "out.png", "--advertise-host", "1.2.3.4"][..],
            &["--preview", "out.png", "--simulator", "http://s:1"],
            &["--blank", "c.toml"],
            &["--blank", "--check"],
            &["--blank", "--simulator", "http://s:1"],
        ] {
            assert_eq!(error(args).kind(), ErrorKind::ArgumentConflict, "{args:?}");
        }
        // The default config path is not a conflict.
        assert_eq!(parse(&["--blank"]), Command::Blank);
    }

    #[test]
    fn bad_values_name_the_choices() {
        let err = error(&["simulator", "--model", "huge"]).to_string();
        assert!(err.contains("mk2, mini, neo, xl, plus"), "{err}");
        let err = error(&["spotify-login", "--listen", "nowhere"]).to_string();
        assert!(err.contains("--listen"), "{err}");
    }

    #[test]
    fn version_and_help_are_answered() {
        assert_eq!(error(&["--version"]).kind(), ErrorKind::DisplayVersion);
        assert_eq!(error(&["-h"]).kind(), ErrorKind::DisplayHelp);
        let text = error(&["--version"]).to_string();
        assert_eq!(
            text.trim(),
            format!("deckjay {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn unknown_options_are_errors() {
        assert_eq!(error(&["--bogus"]).kind(), ErrorKind::UnknownArgument);
        assert_eq!(
            error(&["simulator", "c.toml"]).kind(),
            ErrorKind::UnknownArgument
        );
    }
}
