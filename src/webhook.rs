use std::{path::PathBuf, sync::Arc};

use clap::ArgMatches;
use eyre::{OptionExt, Result};
use rustls::ServerConfig;
use xitca_web::codegen::State;

use crate::{
    mutation::mutate,
    tls_cert_resolver::TLSCertResolver,
    types::{MatchCombiner, Matches},
    validation::validate,
};

#[derive(State)]
pub struct Webhook {
    pub matches: Matches,
    pub combiner: MatchCombiner,
    pub tls_folder: PathBuf,
    pub name: String,
}
impl TryFrom<ArgMatches> for Webhook {
    type Error = eyre::Report;

    fn try_from(mut args: ArgMatches) -> Result<Self> {
        let matches = Matches::try_from(&mut args)?;
        Ok(Self {
            matches,
            combiner: args.remove_one::<MatchCombiner>("match_combiner").unwrap(), // unwrap is guaranteed by default_value
            tls_folder: args
                .remove_one("tls_folder")
                .ok_or_eyre("No tls_folder specified")?,
            name: args.remove_one("name").ok_or_eyre("No name specified")?,
        })
    }
}

impl Webhook {
    pub async fn start(self) -> Result<()> {
        // ca.crt tls.crt tls.key
        let cert_resolver =
            TLSCertResolver::new(self.tls_folder.clone(), "tls.crt", "tls.key").await?;
        let tls_conf = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(cert_resolver);
        // .with_single_cert(certs, key)?;

        xitca_web::App::new()
            .with_state(Arc::new(self))
            .at_typed(validate)
            .at_typed(mutate)
            .serve()
            .enable_io_uring()
            .bind_rustls("0.0.0.0:443", tls_conf)?
            .run()
            .await?;

        Ok(())
    }
}
