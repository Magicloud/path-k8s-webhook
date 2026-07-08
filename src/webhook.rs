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
use eyre::{Report, Result, eyre};
use futures::future::join_all;
use kube::{
    Api, Client, Discovery,
    api::{ApiResource, DynamicObject, GroupVersionKind},
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;

use crate::types::{Contains, Match, MatchCombiner, MatchValue, Webhook};

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
    let mut try_block = || {
        let review =
            serde_json::from_value::<AdmissionReview<DynamicObject>>(admission_review.clone())?;
        let mut req = admission_review
            .get_mut("request")
            .ok_or_else(|| eyre!("Cannot get `request` object"))?
            .take();
        let obj = req
            .get_mut("object")
            .ok_or_else(|| eyre!("Cannot get `object` object"))?
            .take();
        let ar = AdmissionResponse::from(
            &serde_json::from_value::<AdmissionRequest<DynamicObject>>(req)?,
        );
        Ok((review, obj, ar))
    };
    let (mut review, obj, mut ar) = try_block().map_err(|e: Report| e.to_string())?;

    let results = data
        .matches
        .iter()
        .map(async |m| match matching(m, &obj).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{e:}");
                false
            }
        })
        .collect::<Vec<_>>();
    let results: Vec<_> = join_all(results).await;
    let mut results = results.iter();

    let result = match &data.combiner {
        MatchCombiner::Any => results.any(|r| *r),
        MatchCombiner::All => results.all(|r| *r),
        MatchCombiner::BooleanExpression(node) => {
            let mut context = HashMapContext::<DefaultNumericTypes>::new();
            let try_block = || {
                for (i, r) in results.enumerate() {
                    let i = i + 1; // Sugar. So cli argument starts from 1.
                    context.set_value(format!("v{i}"), evalexpr::Value::from(*r))?;
                }
                let b = node.eval_boolean_with_context(&context)?;
                Ok(b) as Result<bool>
            };
            match try_block() {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{e}");
                    false
                }
            }
        }
    };

    if result {
        ar.allowed = true;
    } else {
        ar = ar.deny(format!("fail json path validation {}", data.name));
    }
    review.response = Some(ar);
    review.request = None;
    Ok(Json(review))
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
    let results = m.json_path.resolve(obj)?;

    let check = |target_values: &Value| {
        let result = match m.contains {
            Contains::Equal => target_values == results,
            Contains::Intersect if results.is_array() && target_values.is_array() => {
                let src = results
                    .as_array()
                    .unwrap()
                    .iter()
                    .collect::<HashSet<&Value>>();
                let dst = target_values
                    .as_array()
                    .unwrap()
                    .iter()
                    .collect::<HashSet<&Value>>();
                !src.is_disjoint(&dst)
            }
            Contains::Contain if results.is_array() && target_values.is_array() => {
                let src = results
                    .as_array()
                    .unwrap()
                    .iter()
                    .collect::<HashSet<&Value>>();
                let dst = target_values
                    .as_array()
                    .unwrap()
                    .iter()
                    .collect::<HashSet<&Value>>();
                src.is_subset(&dst) || src.is_superset(&dst)
            }
            Contains::Contain if results.is_array() => {
                let src = results
                    .as_array()
                    .unwrap()
                    .iter()
                    .collect::<HashSet<&Value>>();
                src.contains(target_values)
            }
            Contains::Contain if target_values.is_array() => {
                let dst = target_values
                    .as_array()
                    .unwrap()
                    .iter()
                    .collect::<HashSet<&Value>>();
                dst.contains(results)
            }
            Contains::Contain | Contains::Intersect => {
                Err(eyre!("Source and target are not both list"))?
            }
        };
        Ok(result) as Result<_>
    };

    let result = match &m.value {
        Some(MatchValue::JsonPath {
            json_path,
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
            match &ext_obj {
                Some(None) if *ignore_resource_not_exist => true, // resource required, but not exist and ignore.
                Some(None) => Err(eyre!(""))?, // resource required, but not exist.
                Some(Some(ext_obj)) => {
                    // checking value from resource object
                    let o = json_path.resolve(ext_obj)?;
                    check(o)?
                }
                None => {
                    // checking value from this object
                    let o = json_path.resolve(obj)?;
                    check(o)?
                }
            }
        }
        Some(MatchValue::Value { value }) => check(value)?,
        None => !results.is_null(),
    };

    Ok(result)
}
