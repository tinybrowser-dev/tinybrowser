use std::ffi::OsString;
use std::io;
use std::iter::Peekable;
use std::path::PathBuf;

use browser::{
    AgentOptions, InvalidCa, InvalidIdentity, InvalidProxy, InvalidResolveRule, Profile,
    ProfileError, Proxy, RendererSandbox, TrustedCa, ValidIdentity,
};
use logging::Level;

pub(crate) struct Cli {
    pub(crate) log_level: Option<Level>,
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) verbose: bool,
    pub(crate) sandbox: RendererSandbox,
    pub(crate) mode: Mode,
}

pub(crate) struct DaemonConfig {
    pub(crate) port: Option<u16>,
    pub(crate) profile: Profile,
    pub(crate) network: AgentOptions,
    pub(crate) forward_args: Vec<OsString>,
}

pub(crate) enum Mode {
    Start(Box<DaemonConfig>),
    Serve(Box<DaemonConfig>),
    Stop { profile: Profile },
    Status { profile: Profile },
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
    #[error("missing command; expected start, stop, status, or serve")]
    MissingCommand,
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

Usage: tinybrowser <COMMAND> [OPTIONS]

Commands:
  start   Start the browser daemon in the background and wait until ready
  status  Report daemon status and endpoint as JSON (exits 1 when stopped)
  stop    Gracefully stop the running daemon for a profile
  serve   Run the browser daemon in the foreground (for supervisors / containers)

Workflow:
  1. Start the browser once per profile (stays open across tabs and commands):
       tinybrowser start
     Waits until the server is bound and a renderer is verified ready, then prints:
       {"profile":"default","pid":12345,"host":"127.0.0.1","port":41234,"url":"http://127.0.0.1:41234"}
     Running `tinybrowser start` again for the same profile reuses the running daemon.
  2. Connect any CDP or W3C WebDriver client to `http://127.0.0.1:<port>`:
     - Playwright / CDP: `chromium.connectOverCDP("http://127.0.0.1:<port>")`
       One blank page target is pre-created at startup (`browser.contexts()[0].pages()[0]`).
     - W3C WebDriver: `POST http://127.0.0.1:<port>/session` with body `{"capabilities":{}}`
  3. Inspect or stop explicitly when done:
       tinybrowser status
       tinybrowser stop

Endpoints (http://127.0.0.1:<port>):
  GET  /json/version         CDP browser metadata and `webSocketDebuggerUrl`
  GET  /json/list            CDP open page targets (alias: `GET /json`)
  WS   /devtools/browser     CDP browser WebSocket (flattened `sessionId` routing)
  WS   /devtools/page/<id>   CDP page target WebSocket
  GET  /status               WebDriver readiness probe (`{"value":{"ready":true,"message":"ready"}}`)
  POST /session              WebDriver Classic create session (one active session at a time)
  *    /session/<id>/...     WebDriver Classic session commands (`url`, `execute/sync`, `execute/async`,
                             `element`, `elements`, `screenshot`, `window`, `cookie`, `timeouts`,
                             `actions`, `permissions`)
  WS   /session              WebDriver BiDi WebSocket (standalone BiDi session)
  WS   /session/<id>         WebDriver BiDi WebSocket attached to an existing Classic session

Request requirements:
  - `Host` header must name a loopback host (`127.0.0.1`, `localhost`, or `[::1]`).
  - Omit the `Origin` header (requests with `Origin` return `403 Forbidden`).
  - Browser Fetch Metadata headers indicating a page request (`Sec-Fetch-Site` other than `none`,
    `Sec-Fetch-Mode` other than `cors`, `Sec-Fetch-Dest` other than `empty`, or `Sec-Fetch-User`)
    return `403 Forbidden`.
  - WebDriver `POST` requests with a `Content-Type` header must use `application/json`.

Options (`start` and `serve`):
      --profile <NAME>
          Profile name (`[A-Za-z0-9._-]+`, max 64 bytes) [default: default]
          Also accepted by `status` and `stop`.
      --port <PORT>
          Bind `127.0.0.1:<PORT>` (`0..=65535`; default `0` selects an ephemeral port)
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
      --no-sandbox
          Disable the renderer OS process sandbox (Landlock/seccomp, Seatbelt, Job Object).
          Dangerous: use only inside an already-isolated container or sandbox where the host
          kernel does not permit the renderer sandbox.
      --renderer
          Internal: run a child renderer worker over inherited file descriptor 0.
          Spawned automatically by the daemon; cannot be combined with commands or daemon options.
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
  0  Success (`--help`, `--version`, `start`, `stop`, healthy `status`, or clean `serve` exit)
  1  Runtime, renderer startup, bind, or I/O failure, or `status` when daemon is stopped/unhealthy
  2  Command-line usage or flag validation error (details written to stderr)
"#;

pub(crate) fn parse(args: impl Iterator<Item = OsString>) -> Result<Outcome, CliError> {
    Scanner::new(args).scan()
}

struct Scanner<I: Iterator<Item = OsString>> {
    args: Peekable<I>,
    saw_any: bool,
    end_of_flags: bool,
    command: Option<Command>,
    log_level: Option<Level>,
    log_file: Option<PathBuf>,
    verbose: bool,
    sandbox: Option<RendererSandbox>,
    profile: Option<Profile>,
    port: Option<u16>,
    user_agent: Option<ValidIdentity>,
    network: AgentOptions,
    tls_ca: Vec<PathBuf>,
    has_daemon_only_flags: Option<&'static str>,
    forward_args: Vec<OsString>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Start,
    Serve,
    Stop,
    Status,
    Renderer,
}

impl<I: Iterator<Item = OsString>> Scanner<I> {
    fn new(args: I) -> Self {
        Self {
            args: args.peekable(),
            saw_any: false,
            end_of_flags: false,
            command: None,
            log_level: None,
            log_file: None,
            verbose: false,
            sandbox: None,
            profile: None,
            port: None,
            user_agent: None,
            network: AgentOptions::default(),
            tls_ca: Vec::new(),
            has_daemon_only_flags: None,
            forward_args: Vec::new(),
        }
    }

    fn scan(mut self) -> Result<Outcome, CliError> {
        while let Some(arg) = self.args.next() {
            self.saw_any = true;
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
            return self.long(text, long);
        }
        if let Some(short) = text.strip_prefix('-')
            && !short.is_empty()
        {
            return Self::short(short);
        }
        self.subcommand(text)
    }

    fn subcommand(&mut self, text: &str) -> Result<Option<Outcome>, CliError> {
        if self.command.is_some() {
            return Err(CliError::UnexpectedArgument(text.to_owned()));
        }
        let parsed = match text {
            "start" => Command::Start,
            "serve" => Command::Serve,
            "stop" => Command::Stop,
            "status" => Command::Status,
            _ => return Err(CliError::UnexpectedArgument(text.to_owned())),
        };
        self.command = Some(parsed);
        Ok(None)
    }

    fn long(&mut self, raw_token: &str, long: &str) -> Result<Option<Outcome>, CliError> {
        let (name, inline) = match long.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (long, None),
        };
        match name {
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
            "renderer" => {
                if inline.is_some() {
                    return Err(CliError::UnexpectedValue { flag: "renderer" });
                }
                match self.command {
                    None => {
                        self.command = Some(Command::Renderer);
                        Ok(None)
                    }
                    Some(Command::Renderer) => Err(CliError::Repeated {
                        spelling: "--renderer",
                    }),
                    Some(Command::Start | Command::Serve | Command::Stop | Command::Status) => {
                        Err(unexpected("renderer"))
                    }
                }
            }
            "log-level" => {
                let raw = self.value("log-level", "<LEVEL>", inline)?;
                if self.log_level.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--log-level <LEVEL>",
                    });
                }
                self.log_level = Some(raw.parse()?);
                self.record_forward(raw_token, inline, &raw);
                Ok(None)
            }
            "log-file" => {
                let raw = self.value("log-file", "<PATH>", inline)?;
                if self.log_file.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--log-file <PATH>",
                    });
                }
                self.log_file = Some(PathBuf::from(&raw));
                self.record_forward(raw_token, inline, &raw);
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
                self.forward_args.push(OsString::from(raw_token));
                Ok(None)
            }
            "no-sandbox" => {
                if inline.is_some() {
                    return Err(CliError::UnexpectedValue { flag: "no-sandbox" });
                }
                if self.sandbox.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--no-sandbox",
                    });
                }
                self.sandbox = Some(RendererSandbox::Disabled);
                self.forward_args.push(OsString::from(raw_token));
                Ok(None)
            }
            "profile" => {
                let raw = self.value("profile", "<NAME>", inline)?;
                let profile = Profile::parse(&raw)?;
                if self.profile.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--profile <NAME>",
                    });
                }
                self.profile = Some(profile);
                self.record_forward(raw_token, inline, &raw);
                Ok(None)
            }
            _ => self.daemon_flag(raw_token, name, inline),
        }
    }

    fn daemon_flag(
        &mut self,
        raw_token: &str,
        name: &str,
        inline: Option<&str>,
    ) -> Result<Option<Outcome>, CliError> {
        let flag_name = match name {
            "port" => {
                let raw = self.value("port", "<PORT>", inline)?;
                let port: u16 = raw
                    .parse()
                    .map_err(|_| CliError::InvalidPort(raw.clone()))?;
                if self.port.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--port <PORT>",
                    });
                }
                self.port = Some(port);
                self.record_forward(raw_token, inline, &raw);
                "port"
            }
            "resolve" => {
                let raw = self.value("resolve", "<PATTERN=ADDR>", inline)?;
                let rule = raw.parse()?;
                self.network.resolve.push(rule);
                self.record_forward(raw_token, inline, &raw);
                "resolve"
            }
            "tls-ca" => {
                let raw = self.value("tls-ca", "<PATH>", inline)?;
                self.tls_ca.push(PathBuf::from(&raw));
                self.record_forward(raw_token, inline, &raw);
                "tls-ca"
            }
            "proxy" => {
                let raw = self.value("proxy", "<URI>", inline)?;
                let proxy = raw.parse::<Proxy>()?;
                if self.network.proxy.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--proxy <URI>",
                    });
                }
                self.network.proxy = Some(proxy);
                self.record_forward(raw_token, inline, &raw);
                "proxy"
            }
            "user-agent" => {
                let raw = self.value("user-agent", "<VALUE>", inline)?;
                let identity = ValidIdentity::parse_spec(&raw)?;
                if self.user_agent.is_some() {
                    return Err(CliError::Repeated {
                        spelling: "--user-agent <VALUE>",
                    });
                }
                self.user_agent = Some(identity);
                self.record_forward(raw_token, inline, &raw);
                "user-agent"
            }
            _ => return Err(unexpected(name)),
        };
        if self.has_daemon_only_flags.is_none() {
            self.has_daemon_only_flags = Some(flag_name);
        }
        Ok(None)
    }

    fn record_forward(&mut self, raw_token: &str, inline: Option<&str>, value: &str) {
        self.forward_args.push(OsString::from(raw_token));
        if inline.is_none() {
            self.forward_args.push(OsString::from(value));
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

    fn finish(mut self) -> Result<Outcome, CliError> {
        if !self.saw_any {
            return Ok(Outcome::Help);
        }
        let Some(command) = self.command else {
            return Err(CliError::MissingCommand);
        };
        let mode = match command {
            Command::Renderer => {
                if let Some(flag) = self.has_daemon_only_flags {
                    return Err(unexpected(flag));
                }
                if self.profile.is_some() {
                    return Err(unexpected("profile"));
                }
                Mode::Renderer
            }
            Command::Stop | Command::Status => {
                if let Some(flag) = self.has_daemon_only_flags {
                    return Err(unexpected(flag));
                }
                if self.log_level.is_some() {
                    return Err(unexpected("log-level"));
                }
                if self.log_file.is_some() {
                    return Err(unexpected("log-file"));
                }
                if self.verbose {
                    return Err(unexpected("verbose"));
                }
                if self.sandbox.is_some() {
                    return Err(unexpected("no-sandbox"));
                }
                let profile = self.profile.unwrap_or_default();
                if command == Command::Stop {
                    Mode::Stop { profile }
                } else {
                    Mode::Status { profile }
                }
            }
            Command::Start | Command::Serve => {
                if let Some(identity) = self.user_agent {
                    self.network.identity = identity;
                }
                for path in self.tls_ca {
                    self.network.tls_cas.push(read_ca(&path)?);
                }
                let config = Box::new(DaemonConfig {
                    port: self.port,
                    profile: self.profile.unwrap_or_default(),
                    network: self.network,
                    forward_args: self.forward_args,
                });
                if command == Command::Start {
                    Mode::Start(config)
                } else {
                    Mode::Serve(config)
                }
            }
        };
        let sandbox = self.sandbox.unwrap_or(RendererSandbox::Enabled);
        Ok(Outcome::Run(Box::new(Cli {
            log_level: self.log_level,
            log_file: self.log_file,
            verbose: self.verbose,
            sandbox,
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
