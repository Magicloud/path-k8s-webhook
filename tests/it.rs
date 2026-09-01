#![warn(clippy::cargo)]
#![warn(clippy::complexity)]
#![warn(clippy::correctness)]
#![warn(clippy::nursery)]
#![warn(clippy::pedantic)]
#![warn(clippy::perf)]
#![warn(clippy::style)]
#![warn(clippy::suspicious)]

mod common;

use std::sync::Arc;

use async_drop::Dropper;
use clap::Parser;
use eyre::Result;
use eyre::eyre;
use kube::api::DynamicObject;
use kube::{Client, Discovery};
use libtest_mimic::Trial;
use tokio::sync::{OnceCell, SetError};
// use tracing::level_filters::LevelFilter;
// use tracing_error::ErrorLayer;
// use tracing_subscriber::{
//     fmt::{self, format::FmtSpan},
//     layer::SubscriberExt,
//     util::SubscriberInitExt,
// };

#[allow(clippy::wildcard_imports)]
use crate::common::*;

static DISCOVERY: OnceCell<Discovery> = OnceCell::const_new();
const PERSIST_TEST_ARGS: [&str; 5] = ["webhook", "-n", "ingress-tls", "-t", "/tls/"];

#[tokio::main]
async fn main() -> Result<()> {
    let it_args = ITArgs::parse();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Cannot initialize AWS LC");
    // tracing_subscriber::registry()
    //     .with(LevelFilter::DEBUG)
    //     .with(fmt::layer().with_span_events(FmtSpan::NONE))
    //     .with(ErrorLayer::default())
    //     .try_init()?;
    let client = Client::try_default().await?;
    // run_aggregate does not work with K3S
    let dis = Discovery::new(client.clone()).run().await?;
    DISCOVERY.set(dis).map_err(|e| match e {
        SetError::AlreadyInitializedError(_) => eyre!("DISCOVERY AlreadyInitializedError"),
        SetError::InitializingError(_) => eyre!("DISCOVERY InitializingError"),
    })?;

    let (infra, webhook, args) = prepare_infra(&it_args.image_sha256)?;

    let created_namespace = confirm_namespace(&it_args.namespace, client.clone()).await?;

    let ns = Arc::new(it_args.namespace);
    let x = ns.clone();
    let x = Dropper::new(Guard {
        created_namespace: &created_namespace,
        namespace: x.as_ref(),
        everything: &infra,
    });

    if let Err((e, _done)) =
        create_many_in_namespace(&infra, ns.clone().as_ref(), client.clone()).await
    {
        eprintln!("{e:?}");
        panic!("Creating infrastructure failed");
    }
    eprintln!("Created infra");

    let test_file = include_str!("test.yaml");
    let pass: DynamicObject = serde_yaml_ng::from_str(test_file)?;

    let tests = [
        {
            let mut fail = pass.clone();
            fail.data["spec"]["selector"]["app"] = "not-whoami".into();
            Test {
                name: "match value",
                argument: vec![
                    "-j",
                    "/spec/selector/app",
                    "-v",
                    "\"whoami\"",
                    "-d",
                    "Service",
                ],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["selector"]
                .as_object_mut()
                .and_then(|x| x.remove("app"));
            Test {
                name: "path exists",
                argument: vec!["-j", "/spec/selector/app", "-d", "Service"],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut p = pass.clone();
            p.data["spec"]["selector"]
                .as_object_mut()
                .and_then(|x| x.insert("app1".into(), "whoami".into()));
            let mut fail = pass.clone();
            fail.data["spec"]["selector"]
                .as_object_mut()
                .and_then(|x| x.insert("app1".into(), "not-whoami".into()));
            Test {
                name: "match path",
                argument: vec![
                    "-j",
                    "/spec/selector/app",
                    "-d",
                    "Service",
                    "-p",
                    "/spec/selector/app1",
                ],
                pass: vec![p],
                fail: vec![fail],
            }
        },
        {
            let p: DynamicObject = serde_yaml_ng::from_str(
                r"apiVersion: v1
kind: ConfigMap
metadata:
  name: match-resource
  namespace: test
data:
  app: whoami",
            )?;
            let mut fail = pass.clone();
            fail.data["spec"]["selector"]["app"] = "not-whoami".into();
            Test {
                name: "match resource",
                argument: vec![
                    "-j",
                    "/spec/selector/app",
                    "-d",
                    "Service",
                    "-r",
                    "ConfigMap:test/match-resource",
                    "-p",
                    "/data/app",
                ],
                pass: vec![p.clone(), pass],
                fail: vec![p, fail],
            }
        },
    ]
    .into_iter()
    .map(|t| {
        Trial::test(t.name, {
            let x = webhook.clone();
            let y = client.clone();
            let z = ns.clone();
            let a = args.clone();
            move || do_the_work(x.as_ref(), &a, &t, z.as_ref(), &y)
        })
    })
    .collect();

    let mut args = it_args.mimic_args.clone();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Interupted");
        }
        conclusion = tokio::task::spawn_blocking(move || {
            args.test_threads = Some(1);
            libtest_mimic::run(&args, tests)
        }) => {
            match conclusion {
                Ok(c) => eprintln!("{c:?}"),
                Err(e) => eprintln!("{e:?}"),
            }
        }
    }

    drop(x);
    Ok(())
}
