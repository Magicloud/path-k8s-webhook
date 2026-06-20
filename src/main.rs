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

    // let log_provider = match opentelemetry_otlp::LogExporter::builder()
    //     .with_tonic()
    //     .build()
    // {
    //     Ok(log_exporter) => SdkLoggerProvider::builder()
    //         .with_resource(Resource::builder().with_service_name("ingress-tls").build())
    //         .with_batch_exporter(log_exporter)
    //         .build(),
    //     Err(e) => {
    //         eprintln!("Cannot initialize OTLP log exporter: {e:?}");
    //         SdkLoggerProvider::builder()
    //             .with_batch_exporter(opentelemetry_stdout::LogExporter::default())
    //             .build()
    //     }
    // };
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
