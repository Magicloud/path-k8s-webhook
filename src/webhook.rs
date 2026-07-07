use std::{collections::HashSet, net::SocketAddr, sync::Arc};

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
use evalexpr::{ContextWithMutableVariables, DefaultNumericTypes, HashMapContext};
use eyre::{Result, eyre};
use futures::future::try_join_all;
use jsonpath_rust::query::{QueryRef, js_path_process};
use kube::{
    Api, Client, Discovery,
    api::{ApiResource, DynamicObject, GroupVersionKind},
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;

use crate::types::{Match, MatchCombiner, MatchValue, Webhook};

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

async fn validate(
    State(data): State<Arc<Webhook>>,
    Json(mut admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    let mut try_block = async || {
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
            .map(async |m| matching(m, obj).await)
            .collect::<Vec<_>>();
        let results: Vec<_> = try_join_all(results).await?;
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
    match try_block().await {
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

async fn matching(m: &Match, obj: &Value) -> Result<bool> {
    let results = js_path_process(&m.json_path, obj)?;

    let result = match m.value.as_ref() {
        Some(MatchValue::JsonPath {
            json_path,
            all_must_match,
            resource,
            ignore_resource_not_exist,
        }) => {
            let ext_obj = if let Some(r) = resource {
                let client = Client::try_default().await?;
                // run_aggregated does not work for K3S.
                // let dis = Discovery::new(client.clone()).run_aggregated().await?;
                let dis = Discovery::new(client.clone()).run().await?;
                let target_obj = if let Some(gvk) = dis.groups().find_map(|g| {
                    g.resources_by_stability()
                        .iter()
                        .find(|(ar, _)| ar.kind.eq_ignore_ascii_case(&r.kind))
                        .map(|(ar, _)| GroupVersionKind::gvk(g.name(), &ar.version, &ar.kind))
                }) {
                    eprintln!("{gvk:?}");
                    let ar = ApiResource::from_gvk(&gvk);
                    let api: Api<DynamicObject> = if let Some(ref ns) = r.namespace {
                        Api::namespaced_with(client, ns, &ar)
                    } else {
                        Api::default_namespaced_with(client, &ar)
                    };
                    let dyn_obj = api.get_opt(&r.name).await?;
                    dyn_obj.map(serde_json::to_value).transpose()?
                } else {
                    Err(eyre!(""))?;
                    None
                };
                Some(target_obj)
            } else {
                None
            };

            let check = |o| {
                let target_values = js_path_process(json_path, o)?;
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
                Ok(
                    (*all_must_match && src.len() == dst.len() && src.is_subset(&dst)) // equal
                            || !(*all_must_match || src.is_disjoint(&dst)), // intersect or equal
                ) as Result<_>
            };
            match ext_obj {
                Some(None) if *ignore_resource_not_exist => true,
                Some(None) => Err(eyre!(""))?,
                Some(Some(ext_obj)) => check(&ext_obj)?,
                None => check(obj)?,
            }
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
}
