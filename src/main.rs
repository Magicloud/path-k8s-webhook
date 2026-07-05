// #![warn(clippy::cargo)]
// #![warn(clippy::complexity)]
// #![warn(clippy::correctness)]
// #![warn(clippy::nursery)]
// #![warn(clippy::pedantic)]
// #![warn(clippy::perf)]
// #![warn(clippy::style)]
// #![warn(clippy::suspicious)]
// #![allow(clippy::future_not_send)]
// #![allow(clippy::multiple_crate_versions)]
// #![allow(clippy::wildcard_dependencies)]

mod cli;
mod webhook;

use clap::CommandFactory;
use eyre::{Result, eyre};
use mimalloc::MiMalloc;

use crate::{cli::Cli, webhook::Webhook};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Cannot initialize AWS LC");

    color_eyre::install()?;

    let root_matches = Cli::command().get_matches();

    match root_matches.subcommand() {
        Some(("webhook", args)) => {
            let webhook = Webhook::try_from(args)?;
            drop(root_matches);
            webhook.start().await?;
        }
        _ => unimplemented!(),
    }

    Ok(())
}
