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
mod k8s_renew_watch;
mod mutation;
mod types;
mod validation;
mod webhook;

use clap::CommandFactory;
use eyre::Result;
use mimalloc::MiMalloc;
use tracing::info;
use tracing_error::ErrorLayer;
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::{cli::Cli, webhook::Webhook};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Cannot initialize AWS LC");

    color_eyre::install()?;

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_span_events(FmtSpan::NONE)
                .with_filter(EnvFilter::from_default_env()),
        )
        .with(ErrorLayer::default())
        .try_init()?;

    let mut root_matches = Cli::command().get_matches();

    match root_matches.remove_subcommand() {
        Some((cmd, args)) if cmd == "webhook" => {
            info!("Starting webhook server");
            let webhook = Webhook::try_from(args)?;
            webhook.start().await?;
        }
        _ => unimplemented!(),
    }

    Ok(())
}
