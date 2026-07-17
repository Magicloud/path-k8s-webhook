use axum::Json;
use eyre::{OptionExt, Result};
use kube::{
    api::DynamicObject,
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;
use tracing::{info, warn};

pub async fn boilerplate<F>(
    name: &str,
    mut admission_review: Value,
    f: F,
) -> Result<Json<AdmissionReview<DynamicObject>>, String>
where
    F: AsyncFnOnce(&String, &Value, AdmissionResponse) -> Result<AdmissionResponse>,
{
    let mut try_block = || {
        let review =
            serde_json::from_value::<AdmissionReview<DynamicObject>>(admission_review.clone())?;
        let mut req = admission_review
            .get_mut("request")
            .ok_or_eyre("Cannot get `request` object")?
            .take();
        let kind = review
            .request
            .as_ref()
            .map(|r| r.kind.kind.clone())
            .ok_or_eyre("Cannot get object kind")?;
        let obj = req
            .get_mut("object")
            .ok_or_eyre("Cannot get `object` object")?
            .take();
        let ar = AdmissionResponse::from(
            &serde_json::from_value::<AdmissionRequest<DynamicObject>>(req)?,
        );
        Ok((review, kind, obj, ar))
            as Result<(
                AdmissionReview<DynamicObject>,
                String,
                Value,
                AdmissionResponse,
            )>
    };
    match try_block() {
        Ok((mut review, kind, obj, mut ar)) => {
            let tmp = ar.clone();
            let ar = match f(&kind, &obj, tmp).await {
                Ok(mut ar) => {
                    ar.allowed = true;
                    ar
                }
                Err(e) => {
                    info!("{e:?}");
                    ar = ar.deny(format!("fail json path validation/mutation on {name}"));
                    ar
                }
            };

            review.response = Some(ar);
            review.request = None;
            Ok(Json(review))
        }
        Err(e) => {
            warn!("{e:?}");
            Err(e.to_string())
        }
    }
}
