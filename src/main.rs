use std::error::Error;
use std::fmt::Display;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;

use axum::response::Redirect;
use axum::{Router, routing};
use cargo_metadata::{MetadataCommand, Package, Target};
use clap::Parser;
use log::LevelFilter;
use notify::{Event, EventKind, Watcher};
use tokio::fs;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

#[derive(Parser)]
#[command(version, about = "A minimal live-reload HTTP server for rustdoc.")]
struct Args {
    /// The path to the crate's root, the dir at which Cargo.toml is at
    #[arg(default_value = ".")]
    root: PathBuf,

    /// The packages to generate and serve documentation for.
    ///
    /// Also see `--workspace` and `--exclude`.
    #[arg(short, long)]
    package: Vec<String>,

    /// Generate documentation for all crates in this workspace.
    ///
    /// Also see `--exclude`.
    #[arg(short, long)]
    workspace: bool,

    /// When `--workspace` is set, packages in the workspace not to generate documentation for.
    #[arg(short, long)]
    exclude: Vec<String>,

    /// The address to bind the documentation server to.
    #[arg(short, long, default_value_t = SocketAddr::from(([0, 0, 0, 0], 8000)))]
    bind: SocketAddr,

    /// Open the documentation server on start.
    #[arg(short, long)]
    open: bool,

    /// Also display private modules and items.
    #[arg(short = 'r', long)]
    with_private: bool,
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
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    pretty_logging::init(LevelFilter::Trace, ["docs"]);

    log::info!("Getting cargo metadata...");

    let metadata = MetadataCommand::new()
        .current_dir(&args.root)
        .exec()
        .expect("Failed to get cargo metadata");

    let mut packages = vec![];

    if args.workspace {
        for package in metadata.workspace_packages() {
            if !args.exclude.contains(&package.name) {
                packages.push(package.clone());
            }
        }
    } else if args.package.is_empty() {
        if let Some(package) = metadata.root_package() {
            packages.push(package.clone());
        }
    } else {
        for package in metadata.workspace_packages() {
            if args.package.contains(&package.name) {
                packages.push(package.clone());
            }
        }
    }

    if packages.is_empty() {
        panic!(concat!(
            "No packages of the ones specified were found! Make sure you've specified ",
            "`--package`, `--workspace`, and `--exclude` properly."
        ));
    }

    let Some(target) = find_ideal_target(&packages) else {
        panic!("There was no target to make documentation for!");
    };
    let target = target.clone();

    let package_names: Vec<_> = packages.iter().map(|v| format!("`{}`", v.name)).collect();

    log::info!("Compiling documentation for {}...", list(&package_names));

    let mut cargo_args = vec!["doc".to_string(), "--no-deps".to_string()];

    for package in &packages {
        cargo_args.append(&mut vec!["--package".to_string(), package.name.to_string()]);
    }

    if args.with_private {
        cargo_args.push("--document-private-items".to_string());
    }

    Command::new("cargo")
        .current_dir(&*args.root)
        .args(cargo_args.clone())
        .output()
        .expect("Failed to run `cargo doc`");

    let root = args.root.clone();
    let root_canonical = fs::canonicalize(&root).await?;

    tokio::spawn(async move {
        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

        let mut watcher = notify::recommended_watcher(tx).expect("Failed to create watcher");

        for package in &packages {
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
        }

        for res in rx {
            match res {
                Ok(event) => {
                    match event.kind {
                        EventKind::Create(_) => {
                            for path in event.paths {
                                let relative_path =
                                    pathdiff::diff_paths(path, &root_canonical).unwrap();
                                log::info!("{} created, recompiling...", relative_path.display());
                            }
                        }
                        EventKind::Modify(_) => {
                            for path in event.paths {
                                let relative_path =
                                    pathdiff::diff_paths(path, &root_canonical).unwrap();
                                log::info!("{} changed, recompiling...", relative_path.display());
                            }
                        }
                        EventKind::Remove(_) => {
                            for path in event.paths {
                                let relative_path =
                                    pathdiff::diff_paths(path, &root_canonical).unwrap();
                                log::info!("{} removed, recompiling...", relative_path.display());
                            }
                        }
                        _ => continue,
                    }

                    Command::new("cargo")
                        .current_dir(&*root)
                        .args(cargo_args.clone())
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

    Ok(())
}

fn find_ideal_target(packages: &[Package]) -> Option<&Target> {
    for package in packages {
        for target in &package.targets {
            if target.is_lib() {
                return Some(target);
            }
        }
    }

    for package in packages {
        for target in &package.targets {
            if target.is_bin() {
                return Some(target);
            }
        }
    }

    None
}
/// Formats items into a human-readable list.
///
/// For example,
/// - `[1] => "1"`
/// - `[1, 2] => "1 and 2"`
/// - `[1, 2, 3] => "1, 2, and 3"`.
/// - `[1, 2, 3, 4] => "1, 2, 3, and 4"`.
///
/// Arguments:
/// * `items` - The items of the list.
///
/// Returns:
/// [`String`] -> The formatted list.
fn list<T>(items: &[T]) -> String
where
    T: Display,
{
    let mut string = String::new();

    for (i, item) in items.iter().enumerate() {
        let is_first = i == 0;
        let is_penultimate = i == items.len() - 2;
        let is_last = i == items.len() - 1;

        match (is_first, is_penultimate, is_last) {
            (false, false, false) => string.push_str(&format!("{item}, ")),
            (false, false, true) => string.push_str(&item.to_string()),
            (false, true, false) => string.push_str(&format!("{item}, and ")),
            (false, true, true) => unreachable!(),
            (true, false, false) => string.push_str(&format!("{item}, ")),
            (true, false, true) => string.push_str(&item.to_string()),
            (true, true, false) => string.push_str(&format!("{item} and ")),
            (true, true, true) => unreachable!(),
        }
    }

    string
}
