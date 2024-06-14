#![deny(clippy::all, clippy::pedantic)]
#![allow(clippy::almost_swapped)] // Why is clippy flagging clap?

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;

const MAX_PAR: usize = 10;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("asdf")]
    FetchError(#[from] reqwest::Error),
    #[error("asdf")]
    FileError(#[from] std::io::Error),
}

async fn download_crate(workdir: &Path, name: &str, version: &str) -> Result<(), Error> {
    use bytes::Buf;

    let filename = format!("{name}-{version}.crate");
    let out_file = workdir.join("crates").join(&filename);

    let url = format!("https://static.crates.io/crates/{name}/{filename}");

    log::debug!("Fetching {}", url);
    let response = reqwest::get(url).await?;
    let content = response.bytes().await?;
    let mut reader = content.reader();
    let mut output = std::fs::File::create(out_file)?;

    std::io::copy(&mut reader, &mut output)?;

    log::info!("Downloaded: {}", filename);

    Ok(())
}

async fn spider_crates<P: AsRef<Path>>(
    workdir: P,
    only_most_recent: bool,
    new_only: bool,
) -> Result<(), Error> {
    let index = crates_index::Index::new_cargo_default().unwrap();

    let _ = std::fs::create_dir(workdir.as_ref().join("crates"));

    let sem = Arc::new(Semaphore::new(MAX_PAR));

    for crate_releases in index.crates() {
        let workdir_clone = workdir.as_ref().to_path_buf();

        let versions = if only_most_recent {
            vec![crate_releases.highest_version().clone()]
        } else {
            crate_releases
                .versions()
                .iter()
                .map(crates_index::Version::clone)
                .collect()
        };

        let permit = Arc::clone(&sem).acquire_owned().await;
        tokio::spawn(async move {
            #[allow(unused_variables)]
            let permit = permit;
            for version in versions {
                let filename = format!("{}-{}.crate", version.name(), version.version());
                let out_file = workdir_clone.join("crates").join(&filename);

                if out_file.exists() {
                    if !new_only {
                        // We are downloading all, delete it and refresh it.
                        std::fs::remove_file(&out_file).unwrap();
                    } else {
                        log::debug!("{}-{} exists,skipping..", version.name(), version.version());
                        continue;
                    }
                }

                //  fail
                if let Err(e) =
                    download_crate(&workdir_clone, version.name(), version.version()).await
                {
                    log::error!("{}-{} Failed", version.name(), version.version());
                    log::error!("{:?}", e);
                }
            }
        });
    }

    Ok(())
}

async fn extract_crates<P: AsRef<Path>>(
    workdir: &P,
    limit: Option<usize>,
    update_only: bool,
) -> Result<(), Error> {
    let paths = std::fs::read_dir(workdir.as_ref().join("crates")).unwrap();

    let ex_path = workdir.as_ref().join("sources");
    let _ = std::fs::create_dir(&ex_path);

    let sem = Arc::new(Semaphore::new(MAX_PAR));
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for entry in paths {
        if let Ok(entry) = entry {
            let cratefile = entry.path();
            let ex_path_clone = ex_path.clone();

            let output_folder = ex_path_clone.join(
                cratefile
                    .file_name()
                    .unwrap()
                    .clone()
                    .to_string_lossy()
                    .replace(".crate", ""),
            );

            if count.load(std::sync::atomic::Ordering::SeqCst) > limit.unwrap_or(usize::MAX) {
                return Ok(());
            }
            if update_only && output_folder.exists() {
                log::trace!("{} exists, skipping..", output_folder.display());
                continue;
            }

            log::info!(
                "New: {} -> {}",
                cratefile.display(),
                &output_folder.file_name().unwrap().to_string_lossy()
            );

            let permit = Arc::clone(&sem).acquire_owned().await;
            let count_clone = count.clone();
            tokio::spawn(async move {
                #[allow(unused_variables)]
                let permit = permit;
                let tar_gz = std::fs::File::open(&cratefile).unwrap();
                let tar = flate2::read::GzDecoder::new(tar_gz);
                let mut archive = tar::Archive::new(tar);
                if let Err(e) = archive.unpack(ex_path_clone) {
                    log::error!("Failed extraction of {:?}: {:?}", &cratefile, e);
                }
                log::info!(
                    "Extracted: {}",
                    &output_folder.file_name().unwrap().to_string_lossy()
                );
                count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
    }
    Ok(())
}

async fn yank<P: AsRef<Path>>(workdir: &P) -> Result<(), Error> {
    let index = crates_index::Index::new_cargo_default().unwrap();

    let mut lookup = std::collections::HashSet::with_capacity(2_000_000);
    for c in index.crates() {
        for v in c.versions() {
            lookup.insert(format!("{}-{}", v.name(), v.version()));
        }
    }

    log::debug!("Lookup table built");

    let trash_path = workdir.as_ref().join("trash");
    let _ = std::fs::create_dir(&trash_path);

    // Clean crate files
    let paths = std::fs::read_dir(workdir.as_ref().join("crates")).unwrap();
    for entry in paths {
        if let Ok(entry) = entry {
            let fullname = entry
                .file_name()
                .clone()
                .to_string_lossy()
                .replace(".crate", "");

            if !lookup.contains(&fullname) {
                log::info!("Deleted: {}", &fullname);
                std::fs::rename(entry.path(), trash_path.join(format!("{}.crate", fullname)))
                    .unwrap();
            }
        }
    }

    // Clean sources
    let paths = std::fs::read_dir(workdir.as_ref().join("sources")).unwrap();
    for entry in paths {
        if let Ok(entry) = entry {
            let fullname = entry.file_name().to_string_lossy().to_string();

            if !lookup.contains(&fullname) {
                std::fs::remove_dir_all(&entry.path());
            }
        }
    }

    Ok(())
}

async fn build_latest_links<P: AsRef<Path>>(workdir: &P) -> Result<(), Error> {
    let index = crates_index::Index::new_cargo_default().unwrap();
    let latest_dir = workdir.as_ref().join("latest");

    let _ = std::fs::remove_dir_all(&latest_dir);
    let _ = std::fs::create_dir(&latest_dir);

    for c in index.crates() {
        let latest = c.highest_version();
        let name = format!("{}-{}", latest.name(), latest.version());

        let sources_path = workdir.as_ref().join("sources").join(&name);
        let latest_path = workdir.as_ref().join("latest").join(&name);
        log::info!(
            "Linking: {} -> {}",
            &latest_path.to_string_lossy(),
            &sources_path.to_string_lossy()
        );

        std::os::unix::fs::symlink(&sources_path, &latest_path).unwrap();
    }

    Ok(())
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Spider {
        #[arg(long, short, default_value = "false")]
        only_most_recent: bool,
        #[arg(long, short, default_value = "false")]
        update_only: bool,
    },
    Extract {
        #[arg(long, short)]
        limit: Option<usize>,
        #[arg(long, short, default_value = "false")]
        update_only: bool,
    },
    BuildLatestLinks,
    Yank,
}

#[derive(clap::Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    workdir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    use clap::Parser;

    let args = Args::parse();

    env_logger::init();

    match &args.command {
        Commands::Spider {
            only_most_recent,
            update_only,
        } => spider_crates(&args.workdir, *only_most_recent, *update_only)
            .await
            .unwrap(),
        Commands::Extract { limit, update_only } => {
            extract_crates(&args.workdir, *limit, *update_only)
                .await
                .unwrap()
        }
        Commands::BuildLatestLinks => build_latest_links(&args.workdir).await.unwrap(),
        Commands::Yank => yank(&args.workdir).await.unwrap(),
    }
}
