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
use serde_json::json;
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

    let (mut infra, webhook, args) = prepare_infra(&it_args.image_sha256)?;
    regulate_infra(&mut infra, &it_args.namespace).expect("msg");

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

    let mut margs = it_args.mimic_args.clone();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Interupted");
        }
        conclusion = tokio::task::spawn_blocking(move || {
            margs.test_threads = Some(1);
            libtest_mimic::run(&margs, tests().map(|t| {
        Trial::test(t.name, {
            let x = webhook.clone();
            let y = client.clone();
            let z = ns.clone();
            let a = args.clone();
            move || do_the_work(x.as_ref(), &a, &t, z.as_ref(), &y, true)
        })
    })
    .collect())
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

#[allow(clippy::too_many_lines)]
fn tests() -> impl Iterator<Item = Test<'static>> {
    let test_file = include_str!("test.yaml");
    let pass: DynamicObject = serde_yaml_ng::from_str(test_file).expect("Cannot load test.yaml");

    [
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
data:
    app: whoami",
            )
            .expect("Cannot load testing ConfigMap");
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
                pass: vec![p.clone(), pass.clone()],
                fail: vec![p, fail],
            }
        },
        {
            Test {
                name: "match not-existing resource",
                argument: vec![
                    "-j",
                    "/spec/selector/app",
                    "-d",
                    "Service",
                    "-r",
                    "ConfigMap:test/match-resource",
                    "-p",
                    "/data/app",
                    "-i",
                ],
                pass: vec![pass.clone()],
                fail: vec![],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["loadBalancerSourceRanges"] = json!(["1.1.1.1/32", "3.3.3.3/32"]);
            Test {
                name: "source array equals target array",
                argument: vec![
                    "-j",
                    "/spec/loadBalancerSourceRanges",
                    "-d",
                    "Service",
                    "-v",
                    "[\"1.1.1.1/32\", \"2.2.2.2/32\", \"3.3.3.3/32\"]",
                ],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["loadBalancerSourceRanges"] = json!(["1.1.1.1/32", "3.3.3.3/32"]);
            Test {
                name: "source array contains target value",
                argument: vec![
                    "-j",
                    "/spec/loadBalancerSourceRanges",
                    "-d",
                    "Service",
                    "-v",
                    "\"2.2.2.2/32\"",
                    "-o",
                ],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["loadBalancerSourceRanges"] = json!(["3.3.3.3/32"]);
            Test {
                name: "source array contains target array",
                argument: vec![
                    "-j",
                    "/spec/loadBalancerSourceRanges",
                    "-d",
                    "Service",
                    "-v",
                    "[\"2.2.2.2/32\", \"3.3.3.3/32\"]",
                    "-o",
                ],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["selector"]["app"] = "abcdefg".into();
            Test {
                name: "source value subsumes target array",
                argument: vec![
                    "-j",
                    "/spec/selector/app",
                    "-d",
                    "Service",
                    "-v",
                    "[\"not-whoami\", \"whoami\"]",
                    "-o",
                    "subsume",
                ],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["loadBalancerSourceRanges"] = json!([
                "5.5.5.5/32",
                "1.1.1.1/32",
                "2.2.2.2/32",
                "3.3.3.3/32",
                "4.4.4.4/32"
            ]);
            Test {
                name: "source array subsumes target array",
                argument: vec![
                    "-j",
                    "/spec/loadBalancerSourceRanges",
                    "-d",
                    "Service",
                    "-v",
                    "[\"1.1.1.1/32\", \"2.2.2.2/32\", \"3.3.3.3/32\", \"4.4.4.4/32\"]",
                    "-o",
                    "subsume",
                ],
                pass: vec![pass.clone()],
                fail: vec![fail],
            }
        },
        {
            let mut fail = pass.clone();
            fail.data["spec"]["loadBalancerSourceRanges"] = json!(["5.5.5.5/32", "3.3.3.3/32"]);
            Test {
                name: "source array intersects target array",
                argument: vec![
                    "-j",
                    "/spec/loadBalancerSourceRanges",
                    "-d",
                    "Service",
                    "-v",
                    "[\"1.1.1.1/32\", \"2.2.2.2/32\", \"4.4.4.4/32\"]",
                    "-o",
                    "intersect",
                ],
                pass: vec![pass],
                fail: vec![fail],
            }
        },
    ]
    .into_iter()
}
