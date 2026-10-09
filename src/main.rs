#![warn(clippy::cargo)]
#![warn(clippy::complexity)]
#![warn(clippy::correctness)]
#![warn(clippy::nursery)]
#![warn(clippy::pedantic)]
#![warn(clippy::perf)]
#![warn(clippy::style)]
#![warn(clippy::suspicious)]
#![allow(clippy::multiple_crate_versions)]
#![allow(clippy::missing_errors_doc)]

mod cli;
mod helper;
mod mutation;
mod tls_cert_resolver;
mod types;
mod validation;
mod webhook;

use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use arc_swap::ArcSwap;
use clap::CommandFactory;
use eyre::Result;
use kube::Discovery;
use tokio::runtime::{Builder, LocalOptions};
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
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

static DISCOVERY_CACHE: LazyLock<ArcSwap<(Instant, Option<Discovery>)>> = LazyLock::new(|| {
    ArcSwap::from_pointee((
        Instant::now()
            .checked_sub(Duration::from_hours(24))
            .unwrap(),
        None,
    ))
});

fn main() -> Result<()> {
    let util_runtime = Builder::new_current_thread()
        .enable_all()
        .build_local(LocalOptions::default())?;
    util_runtime.block_on(async_main())?;
    Ok(())
}

async fn async_main() -> Result<()> {
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
