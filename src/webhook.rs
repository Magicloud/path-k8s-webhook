use std::{collections::HashSet, net::SocketAddr, path::PathBuf, sync::Arc};

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
use clap::ArgMatches;
use evalexpr::{
    ContextWithMutableVariables, DefaultNumericTypes, HashMapContext, Node, build_operator_tree,
};
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

use crate::{cli::TypeHelper, helper::chunk_by};

#[derive(Debug)]
pub struct Match {
    pub json_path: JpQuery,
    pub value: Option<MatchValue>,
}

#[derive(Debug)]
pub enum MatchValue {
    Value {
        value: String,
        all_must_match: bool,
    },
    JsonPath {
        json_path: JpQuery,
        all_must_match: bool,
    },
}

#[derive(Debug)]
pub enum MatchCombiner {
    Any,
    All,
    BooleanExpression(Node),
}

pub struct Webhook {
    pub matches: Vec<Match>,
    pub combiner: MatchCombiner,
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
impl TryFrom<ArgMatches> for Webhook {
    type Error = eyre::Report;

    fn try_from(mut args: ArgMatches) -> Result<Self> {
        let json_path = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::JpQueryS).zip(i).collect::<Vec<_>>())
        })("json_path")
        .unwrap_or_default();
        let value = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::String).zip(i).collect::<Vec<_>>())
        })("jp_value")
        .unwrap_or_default();
        let value_json_path = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::JpQueryD).zip(i).collect::<Vec<_>>())
        })("jp_value_json_path")
        .unwrap_or_default();
        let all_must_match = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::Bool).zip(i).collect::<Vec<_>>())
        })("jp_all_must_match")
        .unwrap_or_default();
        let mut prepare = [json_path, value, value_json_path, all_must_match]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        prepare.sort_by_key(|(_, i)| *i);
        let matches = chunk_by(prepare, |(n, _)| !matches!(n, TypeHelper::JpQueryS(_)))
            .into_iter()
            .map(|match_data| {
                let mut query = None;
                let mut all = false;
                let mut value = None;
                let mut target_jp = None;
                for (i, _) in match_data {
                    match i {
                        TypeHelper::JpQueryS(jp_query) => query = Some(jp_query),
                        TypeHelper::JpQueryD(jp_query) => target_jp = Some(jp_query),
                        TypeHelper::String(s) => value = Some(s),
                        TypeHelper::Bool(b) => all = b,
                    }
                }

                let query = query.ok_or_else(|| eyre!(""))?;
                match (value, target_jp) {
                    (None, None) => Ok(Match {
                        json_path: query,
                        value: None,
                    }),
                    (None, Some(v)) => Ok(Match {
                        json_path: query,
                        value: Some(MatchValue::JsonPath {
                            json_path: v,
                            all_must_match: all,
                        }),
                    }),
                    (Some(v), None) => Ok(Match {
                        json_path: query,
                        value: Some(MatchValue::Value {
                            value: v,
                            all_must_match: all,
                        }),
                    }),
                    (Some(_), Some(_)) => Err(eyre!("-v and -p cannot be used together")),
                }
            })
            .collect::<Result<Vec<Match>>>()?;

        Ok(Self {
            matches,
            combiner: match args.remove_one("match_combiner") {
                None => MatchCombiner::Any,
                Some(str) => {
                    if str == "ALL" {
                        MatchCombiner::All
                    } else {
                        MatchCombiner::BooleanExpression(build_operator_tree(str)?)
                    }
                }
            },
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

async fn validate(
    State(data): State<Arc<Webhook>>,
    Json(mut admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    let mut try_block = || {
        let mut review =
            serde_json::from_value::<AdmissionReview<DynamicObject>>(admission_review.clone())?;

        let req = admission_review
            .get_mut("request")
            .ok_or_else(|| eyre!("Cannot get `request` object"))?
            .take();
        let obj = req
            .get("object")
            .ok_or_else(|| eyre!("Cannot get `object` object"))?;

        let results = data
            .matches
            .iter()
            .map(|m| {
                let results = js_path_process(&m.json_path, obj)?;

                let result = match m.value.as_ref() {
                    Some(MatchValue::JsonPath {
                        json_path,
                        all_must_match,
                    }) => {
                        let target_values = js_path_process(json_path, obj)?;
                        let src: HashSet<&Value> = results
                            .into_iter()
                            .map(QueryRef::val)
                            .collect::<HashSet<_>>();
                        let dst = target_values
                            .into_iter()
                            .map(QueryRef::val)
                            .collect::<HashSet<_>>();
                        eprintln!("{src:?}");
                        eprintln!("{dst:?}");
                        (*all_must_match && src.len() == dst.len() && src.is_subset(&dst)) // equal
                            || !(*all_must_match || src.is_disjoint(&dst)) // intersect or equal
                    }
                    Some(MatchValue::Value {
                        value,
                        all_must_match,
                    }) => {
                        let check = |v: QueryRef<Value>| {
                            eprint!(
                                "path({}) value({}) equals target value({})? ",
                                m.json_path,
                                v.clone().val(),
                                value
                            );
                            let r = *v.val() == *value;
                            eprintln!("{r}");
                            r
                        };
                        if *all_must_match {
                            results.into_iter().all(check)
                        } else {
                            results.into_iter().any(check)
                        }
                    }
                    None => {
                        if results.is_empty() {
                            eprintln!("path ({}) not matched", m.json_path);
                            false
                        } else {
                            eprintln!("path ({}) matched", m.json_path);
                            true
                        }
                    }
                };

                Ok(result)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut results = results.iter();

        eprintln!("{:?}", data.combiner);
        let result = match &data.combiner {
            MatchCombiner::Any => results.any(|r| *r),
            MatchCombiner::All => results.all(|r| *r),
            MatchCombiner::BooleanExpression(node) => {
                let mut context = HashMapContext::<DefaultNumericTypes>::new();
                for (i, r) in results.enumerate() {
                    let i = i + 1; // Sugar. So cli argument starts from 1.
                    context.set_value(format!("v{i}"), evalexpr::Value::from(*r))?;
                }
                node.eval_boolean_with_context(&context)?
            }
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
