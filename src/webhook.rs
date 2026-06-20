use std::{net::SocketAddr, sync::Arc};

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
use eyre::{Result, eyre};
use jsonpath_rust::query::{QueryRef, js_path_process};
use kube::{
    api::DynamicObject,
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;

use crate::cli::*;

impl Cli {
    pub async fn start(self) -> Result<()> {
        match self.cmd {
            SubCmd::Webhook(webhook_arguments) => {
                let tls_config = RustlsConfig::from_pem_file(
                    &webhook_arguments.tls_certificate_file_name,
                    &webhook_arguments.tls_private_key_file_name,
                )
                .await?;
                let data = Arc::new(webhook_arguments);
                let app = Router::new()
                    .route("/validate", post(validate))
                    .route("/mutate", post(mutate))
                    .layer(middleware::from_fn(content_type_json))
                    .with_state(data);
                axum_server::bind_rustls(SocketAddr::from(([0, 0, 0, 0], 443)), tls_config)
                    .serve(app.into_make_service())
                    .await?;
            }
        }

        Ok(())
    }
}

async fn validate(
    State(data): State<Arc<WebhookArguments>>,
    Json(mut admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
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
        let results = js_path_process(&data.json_path, obj)?;

        let result = if let Some(ref tv) = data.value {
            let check = |v: QueryRef<Value>| *v.val() == *tv;
            let passed = if data.all_must_match {
                results.into_iter().all(check)
            } else {
                results.into_iter().any(check)
            };

            if passed {
                Ok(())
            } else {
                Err("Value does not match")
            }
        } else {
            if !results.is_empty() {
                Ok(())
            } else {
                Err("JSON path not found")
            }
        };

        let mut ar = AdmissionResponse::from(&serde_json::from_value::<
            AdmissionRequest<DynamicObject>,
        >(req)?);

        match result {
            Ok(_) => ar.allowed = true,
            Err(msg) => ar = ar.deny(msg),
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
