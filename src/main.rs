use std::path::{PathBuf,Path};
use log::{trace, debug, info};
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

    let filename = format!("{}-{}.crate", name, version);
    let out_file = workdir.join("crates").join(&filename);

    if ! out_file.exists() {
        let url = format!("https://static.crates.io/crates/{}/{}",
                          name,
                          filename);

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

async fn spider_crates<P: AsRef<Path>>(workdir: P, only_most_recent: bool) -> Result<(), Error>{
    env_logger::init();

    let index = crates_index::Index::new_cargo_default().unwrap();

    let _ = std::fs::create_dir(workdir.as_ref().join("crates"));

    let sem = Arc::new(Semaphore::new(MAX_PAR));

    for crate_releases in index.crates() {
        let workdir_clone = workdir.as_ref().to_path_buf();
        let _ = crate_releases.most_recent_version(); // newest version

        let versions = if only_most_recent {
            vec![crate_releases.highest_version().clone()]
        } else {
            crate_releases.versions().iter().map(crates_index::Version::clone).collect()
        };

        let permit = Arc::clone(&sem).acquire_owned().await;
        tokio::spawn(async move {
            let _permit = permit;
            trace!("{:?}", versions);
            for version in versions {
                download_crate(&workdir_clone, version.name(), version.version()).await.unwrap();
            }
        });
    }

    Ok(())
}

async fn extract_crates<P: AsRef<Path>>(workdir: &P) -> Result<(), Error> {
    let paths = std::fs::read_dir(workdir.as_ref().join("crates")).unwrap();
    let ex_path = workdir.as_ref().join("sources");

    let _ = std::fs::create_dir(&ex_path);

    let sem = Arc::new(Semaphore::new(MAX_PAR));

    for entry_result in paths {
        if let Ok(entry) = entry_result {
            let cratefile = entry.path().to_path_buf();
            let ex_path_clone = ex_path.clone();
            debug!("Name: {}", cratefile.display());

            let permit = Arc::clone(&sem).acquire_owned().await;
            tokio::spawn(async move {
                let _permit = permit;
                let tar_gz = std::fs::File::open(cratefile).unwrap();
                let tar = flate2::read::GzDecoder::new(tar_gz);
                let mut archive = tar::Archive::new(tar);
                archive.unpack(ex_path_clone).unwrap();
            });
        }
    }
    Ok(())
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Spider {
        #[clap(long, short, action, default_value = "false")]
        only_most_recent: bool,
    },
    Extract
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
            only_most_recent
        } => spider_crates(&args.workdir, *only_most_recent).await.unwrap(),
        Commands::Extract => extract_crates(&args.workdir).await.unwrap(),
    }
}
