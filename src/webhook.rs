use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode},
    middleware::{self, Next},
    routing::post,
};
use axum_extra::headers::{ContentType, HeaderMapExt};
use axum_server::tls_rustls::RustlsConfig;
use clap::ArgMatches;
use eyre::{OptionExt, Result};
use tokio::spawn;
use tracing::{info, warn};

use crate::{
    k8s_renew_watch::MountFolderWatcher,
    mutation::mutate,
    types::{MatchCombiner, Matches},
    validation::validate,
};

pub struct Webhook {
    pub matches: Matches,
    pub combiner: MatchCombiner,
    pub tls_certificate_file_name: PathBuf,
    pub tls_private_key_file_name: PathBuf,
    pub name: String,
}
impl TryFrom<ArgMatches> for Webhook {
    type Error = eyre::Report;

    fn try_from(mut args: ArgMatches) -> Result<Self> {
        let matches = Matches::try_from(&mut args)?;
        Ok(Self {
            matches,
            combiner: args.remove_one::<MatchCombiner>("match_combiner").unwrap(), // unwrap is guaranteed by default_value
            tls_certificate_file_name: args
                .remove_one("tls_certificate_file_name")
                .ok_or_eyre("No tls_certificate_file_name specified")?,
            tls_private_key_file_name: args
                .remove_one("tls_private_key_file_name")
                .ok_or_eyre("No tls_private_key_file_name specified")?,
            name: args.remove_one("name").ok_or_eyre("No name specified")?,
        })
    }
}

impl Webhook {
    pub async fn start(self) -> Result<()> {
        let tls_config = RustlsConfig::from_pem_file(
            &self.tls_certificate_file_name,
            &self.tls_private_key_file_name,
        )
        .await?;

        let x = self.tls_certificate_file_name.parent();
        let y = self.tls_private_key_file_name.parent();
        if x == y {
            // this check is just that one less argument needed to be passed.
            let cert_watcher = MountFolderWatcher {
                mount_folders: vec![x.map(std::path::Path::to_path_buf).unwrap_or_default()].into(),
            };
            let reloader = tls_config.clone();
            let x = Arc::new(self.tls_certificate_file_name.clone());
            let y = Arc::new(self.tls_private_key_file_name.clone());
            spawn(async move {
                cert_watcher
                    .run(move |msg| {
                        info!("Cert renewed. Got {msg:?}");
                        let reloader = reloader.clone();
                        let x = x.clone();
                        let y = y.clone();
                        async move {
                            if let Err(e) =
                                reloader.reload_from_pem_file(x.as_ref(), y.as_ref()).await
                            {
                                warn!("Cert reload error: {e:?}");
                            }
                            Ok(())
                        }
                    })
                    .await
            });
        } else {
            info!("Cannot watch cert files for hot reload");
        }

        let data = Arc::new(self);
        let app = Router::new()
            .route("/validate", post(validate))
            .route("/mutate", post(mutate))
            .layer(middleware::from_fn(content_type_json))
            .with_state(data);
        axum_server::bind_rustls(SocketAddr::from(([0, 0, 0, 0], 443)), tls_config)
            .serve(app.into_make_service())
            .await?;

        Ok(())
    }
}

async fn content_type_json(
    request: Request<Body>,
    next: Next,
) -> Result<Response<Body>, StatusCode> {
    if let Some(ref ct) = request.headers().typed_get::<ContentType>()
        && *ct == ContentType::json()
    {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNSUPPORTED_MEDIA_TYPE)
    }
}
