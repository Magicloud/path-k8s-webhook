use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, Response, StatusCode},
    middleware::{self, Next},
    routing::post,
};
use axum_extra::headers::{ContentType, HeaderMapExt};
use axum_server::tls_rustls::RustlsConfig;
use clap::{ArgMatches, FromArgMatches, parser::ValuesRef};
use eyre::{Result, eyre};
use jsonpath_rust::{
    parser::model::JpQuery,
    query::{QueryRef, js_path_process},
};
use kube::{
    api::DynamicObject,
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;

use crate::cli::{TypeHelper, WebhookArguments};

#[derive(Debug)]
pub struct Match {
    pub json_path: JpQuery,
    pub value: Option<MatchValue>,
}

#[derive(Debug)]
pub struct MatchValue {
    pub value: String,
    pub all_must_match: bool,
}

pub struct Webhook {
    pub matches: Vec<Match>,
    pub all_must_match: bool,
    pub tls_certificate_file_name: PathBuf,
    pub tls_private_key_file_name: PathBuf,
    pub name: String,
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
impl TryFrom<&ArgMatches> for Webhook {
    type Error = eyre::Report;

    fn try_from(args: &ArgMatches) -> Result<Self> {
        let struct_args = WebhookArguments::from_arg_matches(args)?;

        let json_path = (|key| {
            let k: ValuesRef<JpQuery> = args.get_many(key)?;
            let i = args.indices_of(key)?;
            Some(k.map(TypeHelper::from).zip(i).collect::<Vec<_>>())
        })("json_path")
        .unwrap_or_default();
        let value = (|key| {
            let k: ValuesRef<String> = args.get_many(key)?;
            let i = args.indices_of(key)?;
            Some(k.map(TypeHelper::from).zip(i).collect::<Vec<_>>())
        })("jp_value")
        .unwrap_or_default();
        let all_must_match = (|key| {
            let k: ValuesRef<bool> = args.get_many(key)?;
            let i = args.indices_of(key)?;
            Some(k.map(TypeHelper::from).zip(i).collect::<Vec<_>>())
        })("jp_all_must_match")
        .unwrap_or_default();
        let mut prepare = [json_path, value, all_must_match]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        prepare.sort_by_key(|(_, i)| *i);

        let mut i = 0;
        let mut matches = vec![];
        while i < prepare.len() {
            let m = if let Some((TypeHelper::JpQuery(j), _)) = prepare.get(i) {
                i += 1;
                if let Some((TypeHelper::String(v), _)) = prepare.get(i) {
                    i += 1;
                    Match {
                        json_path: (**j).clone(),
                        value: if let Some((TypeHelper::Bool(a), _)) = prepare.get(i) {
                            i += 1;
                            Some(MatchValue {
                                value: (**v).clone(),
                                all_must_match: **a,
                            })
                        } else {
                            Some(MatchValue {
                                value: (**v).clone(),
                                all_must_match: false,
                            })
                        },
                    }
                } else {
                    Match {
                        json_path: (**j).clone(),
                        value: None,
                    }
                }
            } else {
                Err(eyre!(""))?
            };
            matches.push(m);
        }

        Ok(Webhook {
            matches,
            all_must_match: struct_args.all_must_match,
            tls_certificate_file_name: struct_args.tls_certificate_file_name,
            tls_private_key_file_name: struct_args.tls_private_key_file_name,
            name: struct_args.name,
        })
    }
}

async fn validate(
    State(data): State<Arc<Webhook>>,
    Json(mut admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    eprintln!("{:?}", data.matches);
    let mut try_block = || {
        let mut review =
            serde_json::from_value::<AdmissionReview<DynamicObject>>(admission_review.clone())?;

        let req = admission_review
            .get_mut("request")
            .ok_or(eyre!("Cannot get `request` object"))?
            .take();
        let obj = req
            .get("object")
            .ok_or(eyre!("Cannot get `object` object"))?;

        // TODO: logs failures
        let mut results = data.matches.iter().map(|m| {
            let results = js_path_process(&m.json_path, obj)?;
            let result = if let Some(ref tv) = m.value {
                let check = |v: QueryRef<Value>| *v.val() == *tv.value;
                if tv.all_must_match {
                    results.into_iter().all(check)
                } else {
                    results.into_iter().any(check)
                }
            } else {
                !results.is_empty()
            };
            Ok(result)
        });
        let result = if data.all_must_match {
            results.all(|r: Result<bool>| matches!(r, Ok(true)))
        } else {
            results.any(|r| matches!(r, Ok(true)))
        };

        let mut ar = AdmissionResponse::from(&serde_json::from_value::<
            AdmissionRequest<DynamicObject>,
        >(req)?);

        if result {
            ar.allowed = true;
        } else {
            ar = ar.deny(format!("fail json path validation {}", data.name));
        }

        review.response = Some(ar);
        review.request = None;

        Ok(review) as Result<AdmissionReview<DynamicObject>>
    };
    match try_block() {
        Ok(ar) => Ok(Json(ar)),
        Err(e) => {
            eprintln!("{e:?}");
            Err(e.to_string())
        }
    }
}

async fn mutate() -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "")
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
