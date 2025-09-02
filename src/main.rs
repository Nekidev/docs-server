use std::{
    fmt::{Display, Formatter},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use axum::{Router, response::Redirect, routing};
use cargo_metadata::MetadataCommand;
use clap::Parser;
use log::LevelFilter;
use notify::{Event, EventKind, Watcher};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

#[derive(Parser)]
#[command(
    version,
    about = "A minimal live-reload HTTP server for rustdoc."
)]
struct Args {
    /// The path to the crate's root, the dir at which Cargo.toml is at
    #[arg(default_value_t = PathWrapper::new(PathBuf::from_str(".").unwrap()))]
    root: PathWrapper,

    /// The package to generate and serve documentation for.
    #[arg(short, long)]
    package: Option<String>,

    /// The address to bind the documentation server to.
    #[arg(short, long, default_value_t = SocketAddr::from(([0, 0, 0, 0], 8000)))]
    bind: SocketAddr,

    /// Open the documentation server on start.
    #[arg(short, long)]
    open: bool,
}

#[derive(Clone)]
struct PathWrapper {
    path: PathBuf,
}

impl PathWrapper {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl FromStr for PathWrapper {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(PathBuf::from(s)))
    }
}

impl Display for PathWrapper {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.display())
    }
}

impl Deref for PathWrapper {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl DerefMut for PathWrapper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.path
    }
}

fn split_once_last(s: &str, c: char) -> Option<(&str, &str)> {
    s.rfind(c).map(|idx| {
        let (left, right) = s.split_at(idx);
        (left, &right[c.len_utf8()..]) // skip the separator
    })
}

/// Boots up a documentation server.
///
/// It compiles the crate's documentation and recompiles it automatically when the source code
/// changes.
#[tokio::main]
async fn main() {
    let args = Args::parse();

    pretty_logging::init(LevelFilter::Trace, ["docs"]);

    log::info!("Getting cargo metadata...");

    let metadata = MetadataCommand::new()
        .current_dir((*args.root).clone())
        .exec()
        .expect("Failed to get cargo metadata");

    let package = if let Some(package) = args.package {
        metadata.packages.iter().find(|p| *p.name == package).unwrap_or_else(|| panic!("Package `{package}` not found. Are you sure you pointed to the right crate root and package name?"))
    } else {
        metadata
            .root_package()
            .or(metadata.workspace_default_packages().into_iter().next())
            .expect("No package was specified and there was no root package either")
    }.clone();

    let package_name = package.name.clone();

    let target = package
        .targets
        .iter().find(|&target| target.is_lib()).cloned()
        .or(package.targets.iter().find(|&target| target.is_bin()).cloned())
        .or(package.targets.first().cloned())
        .expect("This crate has no targets!");

    log::info!("Compiling documentation for `{package_name}`...");

    Command::new("cargo")
        .current_dir(&*args.root)
        .args([
            "doc",
            "--no-deps",
            "--document-private-items",
            "--package",
            &package_name,
        ])
        .output()
        .expect("Failed to run `cargo doc`");

    let root = args.root.clone();

    tokio::spawn(async move {
        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

        let mut watcher = notify::recommended_watcher(tx).expect("Failed to create watcher");

        watcher
            .watch(
                Path::new(&format!(
                    "{}/src/",
                    split_once_last(package.manifest_path.as_str(), '/')
                        .unwrap()
                        .0
                )),
                notify::RecursiveMode::Recursive,
            )
            .expect("Failed to watch src directory");

        for res in rx {
            match res {
                Ok(event) => {
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                            log::info!("Source files changed, recompiling...");
                        }
                        _ => continue,
                    }

                    Command::new("cargo")
                        .current_dir(&*root)
                        .args([
                            "doc",
                            "--no-deps",
                            "--document-private-items",
                            "--package",
                            &package_name,
                        ])
                        .output()
                        .expect("Failed to run `cargo doc`");
                }
                Err(e) => {
                    log::error!("Watch error: {e:?}");
                }
            }
        }
    });

    log::info!("Starting documentation server on address {}...", args.bind);

    let docs: Router<()> = Router::new()
        .route(
            "/",
            routing::get(|| async move { Redirect::permanent(&format!("/{}/", target.name)) }),
        )
        .fallback_service(ServeDir::new(metadata.target_directory.join("doc")));

    let openable_address = if args.bind.ip() == IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)) {
        format!("http://localhost:{}", args.bind.port())
    } else {
        format!("http://{}/", args.bind)
    };

    let listener = TcpListener::bind(args.bind)
        .await
        .expect("Could not bind to address!");

    log::info!("Documentation server is running on {openable_address}");

    let handle = tokio::spawn(async move {
        axum::serve(listener, docs)
            .await
            .expect("Could not start documentation server!")
    });

    if args.open {
        match open::that(openable_address) {
            Ok(_) => log::info!("Opened documentation in browser!"),
            Err(e) => log::error!("Failed to open documentation in browser: {e}"),
        }
    }

    handle.await.expect("Documentation server task failed!");
}
