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
use eyre::{Result, eyre};
use macroweave::repeat;

use crate::{helper::chunk_by, validation::validate};
use crate::{
    mutation::mutate,
    types::{Contains, Match, MatchCombiner, MatchValue, TypeHelper},
};

pub struct Webhook {
    pub matches: Vec<Arc<Match>>,
    pub combiner: MatchCombiner,
    pub tls_certificate_file_name: PathBuf,
    pub tls_private_key_file_name: PathBuf,
    pub name: String,
}
impl TryFrom<ArgMatches> for Webhook {
    type Error = eyre::Report;

    fn try_from(mut args: ArgMatches) -> Result<Self> {
        repeat!((Key, Wrapper) in [
            (json_path, TypeHelper::PointerBufS),
            (jp_value, TypeHelper::Value),
            (jp_ignore, TypeHelper::BoolI),
            (jp_resource, TypeHelper::Resource),
            (jp_value_json_path, TypeHelper::PointerBufD),
            (jp_contains, TypeHelper::Contains),

            (jp_value_m, TypeHelper::ValueM),
            (jp_resource_m, TypeHelper::ResourceM),
            (jp_value_m_json_path, TypeHelper::PointerBufDM),
        ] {
            let Key = (|key| {
                let i = args.indices_of(key)?.collect::<Vec<_>>();
                let k = args.remove_many(key)?;
                Some(k.map(Wrapper).zip(i).collect::<Vec<_>>())
            })("Key")
            .unwrap_or_default();
        });

        let mut prepare = [
            json_path,
            jp_value,
            jp_ignore,
            jp_resource,
            jp_value_json_path,
            jp_contains,
            jp_value_m,
            jp_resource_m,
            jp_value_m_json_path,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        prepare.sort_by_key(|(_, i)| *i);
        let matches = chunk_by(prepare, |(n, _)| !matches!(n, TypeHelper::PointerBufS(_)))
            .into_iter()
            .map(|match_data| {
                let mut query = None;
                let mut contains = Contains::Equal;
                let mut value = None;
                let mut resource = None;
                let mut ignore = false;
                let mut target_jp = None;

                let mut value_m = None;
                let mut resource_m = None;
                let mut target_jp_m = None;
                for (i, _) in match_data {
                    match i {
                        TypeHelper::PointerBufS(jp_query) => query = Some(jp_query),
                        TypeHelper::PointerBufD(jp_query) => target_jp = Some(jp_query),
                        TypeHelper::Value(v) => value = Some(v),
                        TypeHelper::Resource(r) => resource = Some(r),
                        TypeHelper::BoolI(b) => ignore = b,
                        TypeHelper::Contains(c) => contains = c,
                        TypeHelper::ValueM(v) => value_m = Some(v),
                        TypeHelper::ResourceM(r) => resource_m = Some(r),
                        TypeHelper::PointerBufDM(p) => target_jp_m = Some(p),
                    }
                }

                let query = query.ok_or_else(|| eyre!(""))?;
                let mut m = Match {
                    json_path: query,
                    value: None,
                    contains,
                    value_to_be: None,
                };
                match (value, target_jp) {
                    (None, None) => (),
                    (None, Some(v)) => {
                        m.value = Some(MatchValue::JsonPath {
                            json_path: v,
                            resource,
                            ignore_resource_not_exist: ignore,
                        });
                    }
                    (Some(v), None) => m.value = Some(MatchValue::Value { value: v }),
                    (Some(_), Some(_)) => {
                        Err(eyre!("-v and -p cannot be used together"))?;
                    }
                }
                match (value_m, target_jp_m) {
                    (None, None) => (),
                    (None, Some(v)) => {
                        m.value_to_be = Some(MatchValue::JsonPath {
                            resource: resource_m,
                            ignore_resource_not_exist: false,
                            json_path: v,
                        });
                    }
                    (Some(v), None) => m.value_to_be = Some(MatchValue::Value { value: v }),
                    (Some(_), Some(_)) => {
                        Err(eyre!(
                            "--jp-value-m and --jp-value-m-json-path cannot be used together"
                        ))?;
                    }
                }
                Ok(m.into())
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            matches,
            combiner: args.remove_one::<MatchCombiner>("match_combiner").unwrap(), // unwrap is guaranteed by default_value
            tls_certificate_file_name: args
                .remove_one("tls_certificate_file_name")
                .ok_or_else(|| eyre!("No tls_certificate_file_name specified"))?,
            tls_private_key_file_name: args
                .remove_one("tls_private_key_file_name")
                .ok_or_else(|| eyre!("No tls_private_key_file_name specified"))?,
            name: args
                .remove_one("name")
                .ok_or_else(|| eyre!("No name specified"))?,
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
