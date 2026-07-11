// #![warn(clippy::cargo)]
#![warn(clippy::complexity)]
#![warn(clippy::correctness)]
#![warn(clippy::nursery)]
#![warn(clippy::pedantic)]
#![warn(clippy::perf)]
#![warn(clippy::style)]
#![warn(clippy::suspicious)]
// #![allow(clippy::future_not_send)]
// #![allow(clippy::multiple_crate_versions)]
// #![allow(clippy::wildcard_dependencies)]

mod cli;
mod helper;
mod mutation;
mod types;
mod validation;
mod webhook;

use std::{process::exit, time::Duration};

use clap::CommandFactory;
use eyre::Result;
use mimalloc::MiMalloc;
use tokio::time::sleep;

use crate::{cli::Cli, webhook::Webhook};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Cannot initialize AWS LC");

    color_eyre::install()?;

    let mut root_matches = Cli::command().get_matches();

    match root_matches.remove_subcommand() {
        Some((cmd, args)) if cmd == "webhook" => {
            let webhook = Webhook::try_from(args)?;
            webhook.start().await?;
        }
        _ => unimplemented!(),
    }

    sleep(Duration::from_secs(2)).await;
    exit(0);
}
