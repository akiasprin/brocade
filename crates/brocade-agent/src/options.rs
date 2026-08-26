//! The agent's runtime options, read from the command line and the environment.
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub(crate) command: String,
    pub(crate) server: String,
    pub(crate) token: String,
    pub(crate) state_dir: PathBuf,
    pub(crate) apply_mode: ApplyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyMode {
    StateDir,
    Linux,
}

impl Options {
    pub(crate) fn parse(args: Vec<String>) -> Result<Self, String> {
        Self::parse_with_env(args, |name| env::var(name).ok())
    }

    pub(crate) fn parse_with_env(
        args: Vec<String>,
        mut env_value: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let mut command = None;
        let mut server = env_value("BROCADE_AGENT_SERVER");
        let mut token = env_value("BROCADE_NODE_TOKEN")
            .filter(|value| !value.trim().is_empty())
            .map(TokenSource::Inline)
            .or_else(|| {
                env_value("BROCADE_NODE_TOKEN_FILE")
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| TokenSource::File(PathBuf::from(value.trim())))
            });
        let mut state_dir = env_value("BROCADE_AGENT_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./brocade-agent-state"));
        let mut apply_mode = env_value("BROCADE_AGENT_APPLY")
            .map(|value| parse_apply_mode(&value))
            .transpose()?
            .unwrap_or(ApplyMode::StateDir);

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--server" => {
                    i += 1;
                    server = Some(args.get(i).ok_or("--server requires a value")?.to_owned());
                }
                "--token" => {
                    i += 1;
                    token = Some(TokenSource::Inline(
                        args.get(i).ok_or("--token requires a value")?.to_owned(),
                    ));
                }
                "--token-file" => {
                    i += 1;
                    token = Some(TokenSource::File(PathBuf::from(
                        args.get(i).ok_or("--token-file requires a value")?,
                    )));
                }
                "--state-dir" => {
                    i += 1;
                    state_dir = args
                        .get(i)
                        .map(PathBuf::from)
                        .ok_or("--state-dir requires a value")?;
                }
                "--apply" => {
                    i += 1;
                    apply_mode = parse_apply_mode(
                        args.get(i).ok_or("--apply requires state-dir or linux")?,
                    )?;
                }
                value if value.starts_with("--") => {
                    return Err(format!("unknown option {value}"));
                }
                value => {
                    if command.replace(value.to_owned()).is_some() {
                        return Err(format!("unexpected extra argument {value}"));
                    }
                }
            }
            i += 1;
        }

        Ok(Self {
            command: command.unwrap_or_else(|| "apply-once".to_owned()),
            server: server.ok_or("--server or BROCADE_AGENT_SERVER is required")?,
            token: resolve_token(token.ok_or(
                "--token, --token-file, BROCADE_NODE_TOKEN, or BROCADE_NODE_TOKEN_FILE is required",
            )?)?,
            state_dir,
            apply_mode,
        })
    }
}

#[derive(Debug)]
enum TokenSource {
    Inline(String),
    File(PathBuf),
}

fn resolve_token(source: TokenSource) -> Result<String, String> {
    match source {
        TokenSource::Inline(token) => normalize_token(token, "--token/BROCADE_NODE_TOKEN"),
        TokenSource::File(path) => {
            let token = fs::read_to_string(&path).map_err(|error| {
                format!("failed to read token file {}: {error}", path.display())
            })?;
            normalize_token(token, &format!("token file {}", path.display()))
        }
    }
}

fn normalize_token(token: String, source: &str) -> Result<String, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(format!("{source} must not be empty"));
    }
    Ok(token.to_owned())
}

fn parse_apply_mode(value: &str) -> Result<ApplyMode, String> {
    match value {
        "state-dir" => Ok(ApplyMode::StateDir),
        "linux" => Ok(ApplyMode::Linux),
        value => Err(format!(
            "unknown apply mode {value}; expected state-dir or linux"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, env, fs, path::PathBuf};

    use super::{ApplyMode, Options};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "brocade-agent-options-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No command means apply-once: converge once and leave. Defaulting to the
    /// resident mode would leave a background process nobody knows about after a
    /// manual check, racing the systemd one over the same state.
    #[test]
    fn the_default_command_is_a_single_pass() {
        let options =
            Options::parse_with_env(args(&["--server", "http://c", "--token", "t"]), env(&[]))
                .unwrap();

        assert_eq!(options.command, "apply-once");
        assert_eq!(options.apply_mode, ApplyMode::StateDir);
        assert_eq!(options.state_dir, PathBuf::from("./brocade-agent-state"));
    }

    /// The default apply mode only writes state_dir and does not touch the
    /// machine. The cost of a mistyped option is therefore "nothing happened",
    /// not "this machine's wg and firewall were rewritten".
    #[test]
    fn linux_mode_is_never_the_default() {
        let from_env = Options::parse_with_env(
            args(&["--server", "http://c", "--token", "t"]),
            env(&[("BROCADE_AGENT_APPLY", "linux")]),
        )
        .unwrap();
        assert_eq!(from_env.apply_mode, ApplyMode::Linux, "环境变量该认");

        let error = Options::parse_with_env(
            args(&["--server", "http://c", "--token", "t"]),
            env(&[("BROCADE_AGENT_APPLY", "yes")]),
        )
        .unwrap_err();
        assert!(error.contains("unknown apply mode yes"), "实际 {error}");
    }

    /// The command line wins over the environment. The other way round, what a
    /// person typed would be silently overridden by an invisible systemd
    /// `Environment=`.
    #[test]
    fn the_command_line_wins_over_the_environment() {
        let options = Options::parse_with_env(
            args(&[
                "repair",
                "--server",
                "http://cli",
                "--token",
                "cli-token",
                "--state-dir",
                "/tmp/cli",
                "--apply",
                "linux",
            ]),
            env(&[
                ("BROCADE_AGENT_SERVER", "http://env"),
                ("BROCADE_NODE_TOKEN", "env-token"),
                ("BROCADE_AGENT_STATE_DIR", "/tmp/env"),
                ("BROCADE_AGENT_APPLY", "state-dir"),
            ]),
        )
        .unwrap();

        assert_eq!(options.command, "repair");
        assert_eq!(options.server, "http://cli");
        assert_eq!(options.token, "cli-token");
        assert_eq!(options.state_dir, PathBuf::from("/tmp/cli"));
        assert_eq!(options.apply_mode, ApplyMode::Linux);
    }

    /// Everything can come from the environment — the install script's path.
    #[test]
    fn everything_can_come_from_the_environment() {
        let options = Options::parse_with_env(
            args(&[]),
            env(&[
                ("BROCADE_AGENT_SERVER", "http://env"),
                ("BROCADE_NODE_TOKEN", "env-token"),
                ("BROCADE_AGENT_STATE_DIR", "/var/lib/brocade"),
            ]),
        )
        .unwrap();

        assert_eq!(options.server, "http://env");
        assert_eq!(options.token, "env-token");
        assert_eq!(options.state_dir, PathBuf::from("/var/lib/brocade"));
    }

    /// systemd's `Environment=BROCADE_NODE_TOKEN=` leaves an empty value. Empty
    /// does not count; fall through to _TOKEN_FILE — otherwise the install script
    /// writes the file and the agent still reports an empty token.
    #[test]
    fn an_empty_inline_token_falls_through_to_the_token_file() {
        let dir = temp_dir("empty-inline");
        let token_file = dir.join("node.token");
        fs::write(&token_file, "broc_node_from_file\n").unwrap();

        let options = Options::parse_with_env(
            args(&[]),
            env(&[
                ("BROCADE_AGENT_SERVER", "http://c"),
                ("BROCADE_NODE_TOKEN", "   "),
                ("BROCADE_NODE_TOKEN_FILE", &token_file.display().to_string()),
            ]),
        )
        .unwrap();

        assert_eq!(options.token, "broc_node_from_file");
        let _ = fs::remove_dir_all(dir);
    }

    /// When the token file cannot be read, the error must name the path. Without
    /// it all one learns is "something is wrong with the token", when the real
    /// cause may be systemd's ProtectHome hiding the file from the agent.
    #[test]
    fn a_missing_token_file_names_the_path() {
        let error = Options::parse_with_env(
            args(&["--server", "http://c", "--token-file", "/nope/node.token"]),
            env(&[]),
        )
        .unwrap_err();

        assert!(error.contains("/nope/node.token"), "实际 {error}");
        assert!(error.contains("failed to read token file"), "实际 {error}");
    }

    /// An empty token is not a valid value. Sent as a blank Bearer, the control
    /// plane answers 401 and the investigation veers off toward "were this
    /// machine's credentials revoked".
    #[test]
    fn a_blank_token_is_rejected_wherever_it_came_from() {
        let error =
            Options::parse_with_env(args(&["--server", "http://c", "--token", "  "]), env(&[]))
                .unwrap_err();
        assert!(error.contains("must not be empty"), "实际 {error}");

        let dir = temp_dir("blank-file");
        let token_file = dir.join("node.token");
        fs::write(&token_file, "\n\n").unwrap();
        let error = Options::parse_with_env(
            args(&[
                "--server",
                "http://c",
                "--token-file",
                &token_file.display().to_string(),
            ]),
            env(&[]),
        )
        .unwrap_err();
        assert!(error.contains("must not be empty"), "实际 {error}");
        assert!(error.contains("token file"), "要说清是哪一个来源");

        let _ = fs::remove_dir_all(dir);
    }

    /// A missing server or token must report how to supply it, not just that it
    /// is missing. This message is all one sees when the install script fails.
    #[test]
    fn the_missing_input_error_says_how_to_supply_it() {
        let error = Options::parse_with_env(args(&["--token", "t"]), env(&[])).unwrap_err();
        assert!(
            error.contains("--server or BROCADE_AGENT_SERVER"),
            "实际 {error}"
        );

        let error = Options::parse_with_env(args(&["--server", "http://c"]), env(&[])).unwrap_err();
        assert!(error.contains("--token"), "实际 {error}");
        assert!(error.contains("BROCADE_NODE_TOKEN_FILE"), "实际 {error}");
    }

    /// A misspelled option must stop the parse rather than be swallowed as a
    /// positional. Taken as a command name, `--sever` would start the agent on a
    /// subcommand that does not exist, with server quietly filled in from the
    /// environment.
    #[test]
    fn a_misspelled_option_stops_the_parse() {
        let error = Options::parse_with_env(
            args(&["--sever", "http://c", "--token", "t"]),
            env(&[("BROCADE_AGENT_SERVER", "http://env")]),
        )
        .unwrap_err();
        assert_eq!(error, "unknown option --sever");
    }

    /// Two positionals is a typo. Silently taking the last one would run repair
    /// for `apply-once repair` while the operator believes they ran apply-once.
    #[test]
    fn a_second_positional_argument_is_an_error() {
        let error = Options::parse_with_env(
            args(&[
                "apply-once",
                "repair",
                "--server",
                "http://c",
                "--token",
                "t",
            ]),
            env(&[]),
        )
        .unwrap_err();
        assert_eq!(error, "unexpected extra argument repair");
    }

    /// An option missing its value must name itself. Otherwise the next option is
    /// eaten as that value and the shortfall only surfaces at the end, by which
    /// point the error points far away from the actual typo.
    #[test]
    fn an_option_without_a_value_names_itself() {
        for (missing, expected) in [
            ("--server", "--server requires a value"),
            ("--token", "--token requires a value"),
            ("--token-file", "--token-file requires a value"),
            ("--state-dir", "--state-dir requires a value"),
            ("--apply", "--apply requires state-dir or linux"),
        ] {
            let error = Options::parse_with_env(args(&[missing]), env(&[])).unwrap_err();
            assert_eq!(error, expected);
        }
    }
}
