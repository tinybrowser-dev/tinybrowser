use std::ffi::OsString;
use std::io;
use std::iter::Peekable;
use std::path::PathBuf;

use browser::{
    AgentOptions, InvalidCa, InvalidIdentity, InvalidProxy, InvalidResolveRule, Profile,
    ProfileError, Proxy, TrustedCa, ValidIdentity,
};
use logging::Level;

pub(crate) struct Cli {
    pub(crate) log_level: Option<Level>,
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) verbose: bool,
    pub(crate) mode: Mode,
}

pub(crate) enum Mode {
    Daemon {
        port: Option<u16>,
        profile: Profile,
        network: Box<AgentOptions>,
    },
    Renderer,
}

pub(crate) enum Outcome {
    Help,
    Version,
    Run(Box<Cli>),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError {
    #[error("Invalid UTF-8 was detected in one or more arguments")]
    NotUtf8,
    #[error("a value is required for '--{flag} {placeholder}' but none was supplied")]
    MissingValue {
        flag: &'static str,
        placeholder: &'static str,
    },
    #[error("the argument '{spelling}' was provided more than once")]
    Repeated { spelling: &'static str },
    #[error("unexpected value for '--{flag}'")]
    UnexpectedValue { flag: &'static str },
    #[error("unexpected argument '{0}' found")]
    UnexpectedArgument(String),
    #[error("invalid value '{0}' for '--port <PORT>'")]
    InvalidPort(String),
    #[error(transparent)]
    InvalidProfile(#[from] ProfileError),
    #[error(transparent)]
    InvalidLogLevel(#[from] logging::ParseLevelError),
    #[error(transparent)]
    InvalidResolve(#[from] InvalidResolveRule),
    #[error(transparent)]
    InvalidProxy(#[from] InvalidProxy),
    #[error("--user-agent: {0}")]
    InvalidUserAgent(#[from] InvalidIdentity),
    #[error("--tls-ca {}: {source}", path.display())]
    TlsCa { path: PathBuf, source: TlsCaError },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TlsCaError {
    #[error(transparent)]
    Read(#[from] io::Error),
    #[error(transparent)]
    Parse(#[from] InvalidCa),
}

pub(crate) const HELP: &str = r#"Headless web browser for AI agents (CDP + W3C WebDriver on one loopback port).

Usage: tinybrowser [OPTIONS]

Workflow:
  1. Start the server (runs in the foreground; append `&` or use a supervisor to background it):
     - Profile-locked ephemeral port: `tinybrowser &`
       Acquires `$XDG_RUNTIME_DIR/tinybrowser/<profile>/lock`, binds `127.0.0.1:<port>`,
       and atomically writes `$XDG_RUNTIME_DIR/tinybrowser/<profile>/daemon.json`:
         {"pid":<u32>,"host":"127.0.0.1","port":<u16>}
       Done when `daemon.json` exists and `GET http://127.0.0.1:<port>/status` returns 200.
       A second start for the same locked profile exits 0 immediately.
     - Explicit port: `tinybrowser --port <PORT> &`
       Binds `127.0.0.1:<PORT>` directly (`--port 0` selects an ephemeral port).
       Done when `GET http://127.0.0.1:<PORT>/status` returns 200.
  2. Connect a client to `http://127.0.0.1:<port>`:
     - Playwright / CDP: `chromium.connectOverCDP("http://127.0.0.1:<port>")`
       One blank page target is pre-created at startup (`browser.contexts()[0].pages()[0]`).
     - W3C WebDriver: `POST http://127.0.0.1:<port>/session` with body `{"capabilities":{}}`
       Adopts the open page target as the initial window handle (`w1`).

Endpoints (http://127.0.0.1:<port>):
  GET  /json/version         CDP browser metadata and `webSocketDebuggerUrl`
  GET  /json/list            CDP open page targets (alias: `GET /json`)
  WS   /devtools/browser     CDP browser WebSocket (flattened `sessionId` routing)
  WS   /devtools/page/<id>   CDP page target WebSocket
  GET  /status               WebDriver readiness probe (`{"value":{"ready":true,"message":"ready"}}`)
  POST /session              WebDriver create session (one active session at a time)
  *    /session/<id>/...     WebDriver session commands (`url`, `execute/sync`, `execute/async`,
                             `element`, `elements`, `screenshot`, `window`, `cookie`, `timeouts`,
                             `actions`, `permissions`)

Request requirements:
  - `Host` header must name a loopback host (`127.0.0.1`, `localhost`, or `[::1]`).
  - Omit the `Origin` header (requests with `Origin` return `403 Forbidden`).
  - Browser Fetch Metadata headers indicating a page request (`Sec-Fetch-Site` other than `none`,
    `Sec-Fetch-Mode` other than `cors`, `Sec-Fetch-Dest` other than `empty`, or `Sec-Fetch-User`)
    return `403 Forbidden`.
  - WebDriver `POST` requests with a `Content-Type` header must use `application/json`.

Options:
      --port <PORT>
          Bind `127.0.0.1:<PORT>` (`0..=65535`). When omitted, locks the profile,
          binds an ephemeral port, and writes `daemon.json` in the runtime directory.
      --profile <NAME>
          Profile name (`[A-Za-z0-9._-]+`, max 64 bytes) [default: default]
      --proxy <URI>
          Outbound HTTP/HTTPS proxy (`http://<host>[:<port>]` or `https://<host>[:<port>]`)
      --user-agent <VALUE>
          Browser identity: `tinybrowser` (default), `chrome`, or a custom User-Agent string
      --resolve <PATTERN=ADDR>
          Override DNS resolution (repeatable): `<host|*.domain>=<ip>[:<port>|fail]`
      --tls-ca <PATH>
          Trust an additional PEM CA certificate file (repeatable)
      --log-level <LEVEL>
          Minimum log level: `error`, `warn`, `info`, `debug`, or `trace` [default: info]
          Overrides `TINYBROWSER_LOG`.
      --log-file <PATH>
          Write logs to `<PATH>` instead of `$XDG_DATA_HOME/tinybrowser/logs/<profile>.log`
      --verbose
          Shorthand for `--log-level=debug`
      --renderer
          Internal: run a child renderer worker over inherited file descriptor 0.
          Spawned automatically by the daemon; cannot be combined with daemon options.
  -v, -V, --version
          Print `tinybrowser <version>` to stdout and exit 0
  -h, --help
          Print this help to stdout and exit 0

Paths:
  Runtime endpoint: `$XDG_RUNTIME_DIR/tinybrowser/<profile>/daemon.json`
  Profile lock:     `$XDG_RUNTIME_DIR/tinybrowser/<profile>/lock`
  Profile storage:  `$XDG_DATA_HOME/tinybrowser/profiles/<profile>/`
  Default log file: `$XDG_DATA_HOME/tinybrowser/logs/<profile>.log`

Exit codes:
  0  Clean exit (`--help`, `--version`, `Browser.close`, or profile daemon already running)
  1  Runtime, bind, or I/O failure (details written to the log file)
  2  Command-line usage or flag validation error (details written to stderr)
"#;

pub(crate) fn parse(args: impl Iterator<Item = OsString>) -> Result<Outcome, CliError> {
    Scanner::new(args).scan()
}

struct Scanner<I: Iterator<Item = OsString>> {
    args: Peekable<I>,
    end_of_flags: bool,
    log_level: Option<Level>,
    log_file: Option<PathBuf>,
    verbose: bool,
    mode: Option<Partial>,
}

enum Partial {
    Daemon(Box<DaemonArgs>),
    Renderer,
}

#[derive(Default)]
struct DaemonArgs {
    port: Option<u16>,
    profile: Option<Profile>,
    user_agent_set: bool,
    network: AgentOptions,
    tls_ca: Vec<PathBuf>,
}

impl<I: Iterator<Item = OsString>> Scanner<I> {
    fn new(args: I) -> Self {
        Self {
            args: args.peekable(),
            end_of_flags: false,
            log_level: None,
            log_file: None,
            verbose: false,
            mode: None,
        }
    }

    fn scan(mut self) -> Result<Outcome, CliError> {
        while let Some(arg) = self.args.next() {
            let text = arg.to_str().ok_or(CliError::NotUtf8)?;
            if let Some(outcome) = self.token(text)? {
                return Ok(outcome);
            }
        }
        self.finish()
    }

    fn token(&mut self, text: &str) -> Result<Option<Outcome>, CliError> {
        if self.end_of_flags {
            return Err(CliError::UnexpectedArgument(text.to_owned()));
        }
        if text == "--" {
            self.end_of_flags = true;
            return Ok(None);
        }
        if let Some(long) = text.strip_prefix("--") {
            return self.long(long);
        }
        if let Some(short) = text.strip_prefix('-')
            && !short.is_empty()
        {
            return Self::short(short);
        }
        Err(CliError::UnexpectedArgument(text.to_owned()))
    }

    fn long(&mut self, long: &str) -> Result<Option<Outcome>, CliError> {
        let (name, inline) = match long.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (long, None),
        };
        match name {
            "log-level" => {
                let raw = self.value("log-level", "<LEVEL>", inline)?;
                if self.log_level.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--log-level <LEVEL>",
                    });
                }
                self.log_level = Some(raw.parse()?);
                Ok(None)
            }
            "log-file" => {
                let raw = self.value("log-file", "<PATH>", inline)?;
                if self.log_file.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--log-file <PATH>",
                    });
                }
                self.log_file = Some(PathBuf::from(raw));
                Ok(None)
            }
            "verbose" => {
                if inline.is_some() {
                    return Err(CliError::UnexpectedValue { flag: "verbose" });
                }
                if self.verbose {
                    return Err(CliError::Repeated {
                        spelling: "--verbose",
                    });
                }
                self.verbose = true;
                Ok(None)
            }
            "renderer" => {
                if inline.is_some() {
                    return Err(CliError::UnexpectedValue { flag: "renderer" });
                }
                match self.mode {
                    None => {
                        self.mode = Some(Partial::Renderer);
                        Ok(None)
                    }
                    Some(Partial::Renderer) => Err(CliError::Repeated {
                        spelling: "--renderer",
                    }),
                    Some(Partial::Daemon(_)) => Err(unexpected("renderer")),
                }
            }
            "version" => {
                if inline.is_some() {
                    return Err(CliError::UnexpectedValue { flag: "version" });
                }
                Ok(Some(Outcome::Version))
            }
            "help" => {
                if inline.is_some() {
                    return Err(CliError::UnexpectedValue { flag: "help" });
                }
                Ok(Some(Outcome::Help))
            }
            _ => self.daemon_flag(name, inline),
        }
    }

    fn daemon_args(&mut self, name: &str) -> Result<&mut DaemonArgs, CliError> {
        if matches!(self.mode, Some(Partial::Renderer)) {
            return Err(unexpected(name));
        }
        if self.mode.is_none() {
            self.mode = Some(Partial::Daemon(Box::default()));
        }
        match &mut self.mode {
            Some(Partial::Daemon(daemon)) => Ok(daemon),
            Some(Partial::Renderer) | None => Err(unexpected(name)),
        }
    }

    fn daemon_flag(
        &mut self,
        name: &str,
        inline: Option<&str>,
    ) -> Result<Option<Outcome>, CliError> {
        match name {
            "profile" => {
                let raw = self.value("profile", "<NAME>", inline)?;
                let profile = Profile::parse(&raw)?;
                let daemon = self.daemon_args(name)?;
                if daemon.profile.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--profile <NAME>",
                    });
                }
                daemon.profile = Some(profile);
                Ok(None)
            }
            "port" => {
                let raw = self.value("port", "<PORT>", inline)?;
                let port: u16 = raw.parse().map_err(|_| CliError::InvalidPort(raw))?;
                let daemon = self.daemon_args(name)?;
                if daemon.port.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--port <PORT>",
                    });
                }
                daemon.port = Some(port);
                Ok(None)
            }
            "resolve" => {
                let raw = self.value("resolve", "<PATTERN=ADDR>", inline)?;
                let rule = raw.parse()?;
                let daemon = self.daemon_args(name)?;
                daemon.network.resolve.push(rule);
                Ok(None)
            }
            "tls-ca" => {
                let raw = self.value("tls-ca", "<PATH>", inline)?;
                let daemon = self.daemon_args(name)?;
                daemon.tls_ca.push(PathBuf::from(raw));
                Ok(None)
            }
            "proxy" => {
                let raw = self.value("proxy", "<URI>", inline)?;
                let proxy = raw.parse::<Proxy>()?;
                let daemon = self.daemon_args(name)?;
                if daemon.network.proxy.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--proxy <URI>",
                    });
                }
                daemon.network.proxy = Some(proxy);
                Ok(None)
            }
            "user-agent" => {
                let raw = self.value("user-agent", "<VALUE>", inline)?;
                let identity = ValidIdentity::parse_spec(&raw)?;
                let daemon = self.daemon_args(name)?;
                if daemon.user_agent_set {
                    return Err(CliError::Repeated {
                        spelling: "--user-agent <VALUE>",
                    });
                }
                daemon.user_agent_set = true;
                daemon.network.identity = identity;
                Ok(None)
            }
            _ => Err(unexpected(name)),
        }
    }

    fn short(short: &str) -> Result<Option<Outcome>, CliError> {
        match short {
            "v" | "V" => Ok(Some(Outcome::Version)),
            "h" => Ok(Some(Outcome::Help)),
            _ => Err(CliError::UnexpectedArgument(format!("-{short}"))),
        }
    }

    fn value(
        &mut self,
        flag: &'static str,
        placeholder: &'static str,
        inline: Option<&str>,
    ) -> Result<String, CliError> {
        let missing = || CliError::MissingValue { flag, placeholder };
        if let Some(raw) = inline {
            if raw.is_empty() {
                return Err(missing());
            }
            return Ok(raw.to_owned());
        }
        let Some(next) = self.args.peek() else {
            return Err(missing());
        };
        let Some(text) = next.to_str() else {
            return Err(CliError::NotUtf8);
        };
        if text.starts_with('-') {
            return Err(missing());
        }
        match self.args.next() {
            Some(arg) => arg.into_string().map_err(|_| CliError::NotUtf8),
            None => Err(missing()),
        }
    }

    fn finish(self) -> Result<Outcome, CliError> {
        let mode = match self.mode {
            Some(Partial::Renderer) => Mode::Renderer,
            Some(Partial::Daemon(mut daemon)) => {
                for path in daemon.tls_ca {
                    daemon.network.tls_cas.push(read_ca(&path)?);
                }
                Mode::Daemon {
                    port: daemon.port,
                    profile: daemon.profile.unwrap_or_default(),
                    network: Box::new(daemon.network),
                }
            }
            None => Mode::Daemon {
                port: None,
                profile: Profile::default(),
                network: Box::default(),
            },
        };
        Ok(Outcome::Run(Box::new(Cli {
            log_level: self.log_level,
            log_file: self.log_file,
            verbose: self.verbose,
            mode,
        })))
    }
}

fn unexpected(name: &str) -> CliError {
    CliError::UnexpectedArgument(format!("--{name}"))
}

fn read_ca(path: &std::path::Path) -> Result<TrustedCa, CliError> {
    let pem = std::fs::read(path).map_err(|source| CliError::TlsCa {
        path: path.to_owned(),
        source: TlsCaError::Read(source),
    })?;
    TrustedCa::from_pem(&pem).map_err(|source| CliError::TlsCa {
        path: path.to_owned(),
        source: TlsCaError::Parse(source),
    })
}
