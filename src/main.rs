mod cli;
mod daemon;

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use browser::{Profile, default_data_home};
use cli::{Cli, Mode};
use logging::Level;

fn main() -> ExitCode {
    match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli::Outcome::Version) => {
            println!("tinybrowser {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(cli::Outcome::Help) => {
            print!("{}", cli::HELP);
            ExitCode::SUCCESS
        }
        Ok(cli::Outcome::Run(cli)) => {
            if let Err(error) = install_logger(&cli) {
                eprintln!("error: {error}");
                if let Some(source) = std::error::Error::source(&error) {
                    eprintln!("  caused by: {source}");
                }
                return ExitCode::from(2);
            }
            let code = run(*cli);
            if !logging::flush() {
                logging::error!(target: "logging", "file log did not flush before exit");
            }
            code
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> ExitCode {
    let sandbox = cli.sandbox;
    match cli.mode {
        Mode::Renderer => match browser::child::serve(sandbox) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                logging::error!(target: "renderer", "{error}");
                ExitCode::from(1)
            }
        },
        Mode::Daemon {
            port,
            profile,
            network,
        } => run_browser_process("daemon", async move {
            let home = default_data_home()?;
            daemon::run(&profile, &home, port, *network, sandbox).await
        }),
    }
}

fn install_logger(cli: &Cli) -> Result<(), logging::InstallError> {
    let requested = cli.log_level.or(if cli.verbose {
        Some(Level::Debug)
    } else {
        None
    });
    let level = requested.or_else(env_level).unwrap_or(Level::Info);
    let file = cli.log_file.clone().or_else(|| match &cli.mode {
        Mode::Daemon { profile, .. } => profile_log_file(profile),
        Mode::Renderer => None,
    });
    logging::install(level, file, std::time::SystemTime::now)
}

fn env_level() -> Option<Level> {
    std::env::var("TINYBROWSER_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
}

fn profile_log_file(profile: &Profile) -> Option<PathBuf> {
    let home = default_data_home().ok()?;
    Some(
        home.join("tinybrowser")
            .join("logs")
            .join(format!("{}.log", profile.name().as_str())),
    )
}

fn run_browser_process(
    target: &'static str,
    future: impl Future<Output = io::Result<()>>,
) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            logging::error!(target: target, "runtime failed: {error}");
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(future) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            logging::error!(target: target, "{error}");
            ExitCode::from(1)
        }
    }
}
