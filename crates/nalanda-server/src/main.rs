use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use nalanda_server::AppState;

const USAGE: &str = "usage: nalanda-server [--library DIR] [--bind ADDR]

  --library DIR   library directory (default: $NALANDA_LIBRARY or ~/Nalanda Library)
  --bind ADDR     address to listen on (default: 127.0.0.1:8083)";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut library = std::env::var_os("NALANDA_LIBRARY").map(PathBuf::from);
    let mut bind: SocketAddr = "127.0.0.1:8083".parse()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--library" => library = Some(args.next().context(USAGE)?.into()),
            "--bind" => {
                bind = args
                    .next()
                    .context(USAGE)?
                    .parse()
                    .context("invalid --bind")?
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }
    let library = match library {
        Some(dir) => dir,
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Nalanda Library"))
            .context("set --library or $NALANDA_LIBRARY")?,
    };

    let state = AppState {
        library: Arc::new(nalanda_library::Library::open(&library)?),
        converters: Arc::new(nalanda_convert::Converters::default()),
    };
    tracing::info!("library: {}", library.display());
    nalanda_server::serve(bind, state).await
}
