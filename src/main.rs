#![deny(clippy::all, clippy::pedantic)]
#![allow(clippy::almost_swapped)] // Why is clippy flagging clap?

use log::{debug, info, trace};
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

async fn download_crate(
    workdir: &Path,
    name: &str,
    version: &str,
    new_only: bool,
) -> Result<(), Error> {
    use bytes::Buf;

    let filename = format!("{name}-{version}.crate");
    let out_file = workdir.join("crates").join(&filename);

    if !new_only || !out_file.exists() {
        let url = format!("https://static.crates.io/crates/{name}/{filename}");

        debug!("Fetching {}", url);
        let response = reqwest::get(url).await?;
        let content = response.bytes().await?;
        let mut reader = content.reader();
        let mut output = std::fs::File::create(out_file)?;

        std::io::copy(&mut reader, &mut output)?;

        info!("{}", filename);
    }

    Ok(())
}

async fn spider_crates<P: AsRef<Path>>(
    workdir: P,
    only_most_recent: bool,
    new_only: bool,
) -> Result<(), Error> {
    env_logger::init();

    let index = crates_index::Index::new_cargo_default().unwrap();

    std::fs::create_dir(workdir.as_ref().join("crates")).unwrap();

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
            trace!("{:?}", versions);
            for version in versions {
                download_crate(&workdir_clone, version.name(), version.version(), new_only)
                    .await
                    .unwrap();
            }
        });
    }

    Ok(())
}

async fn extract_crates<P: AsRef<Path>>(workdir: &P) -> Result<(), Error> {
    let paths = std::fs::read_dir(workdir.as_ref().join("crates")).unwrap();
    let ex_path = workdir.as_ref().join("sources");

    std::fs::create_dir(&ex_path).unwrap();

    let sem = Arc::new(Semaphore::new(MAX_PAR));

    for entry in paths.flatten() {
        let cratefile = entry.path();
        let ex_path_clone = ex_path.clone();
        debug!("Name: {}", cratefile.display());

        let permit = Arc::clone(&sem).acquire_owned().await;
        tokio::spawn(async move {
            #[allow(unused_variables)]
            let permit = permit;
            let tar_gz = std::fs::File::open(cratefile).unwrap();
            let tar = flate2::read::GzDecoder::new(tar_gz);
            let mut archive = tar::Archive::new(tar);
            archive.unpack(ex_path_clone).unwrap();
        });
    }
    Ok(())
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Spider {
        #[arg(long, short, default_value = "false")]
        only_most_recent: bool,
        #[arg(long, short, default_value = "true")]
        update_only: bool,
    },
    Extract,
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

    match &args.command {
        Commands::Spider {
            only_most_recent,
            update_only,
        } => spider_crates(&args.workdir, *only_most_recent, *update_only)
            .await
            .unwrap(),
        Commands::Extract => extract_crates(&args.workdir).await.unwrap(),
    }
}
