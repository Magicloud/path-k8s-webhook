use std::sync::Arc;

use axum::{Json, extract::State};
use eyre::{Report, Result, eyre};
use kube::{api::DynamicObject, core::admission::AdmissionReview};
use serde_json::Value;

use crate::{
    helper::{checking, fetch_resource, preprocess},
    types::{Match, MatchValue},
    webhook::Webhook,
};

pub async fn mutate(
    State(data): State<Arc<Webhook>>,
    Json(admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    let (mut review, obj, ar) = preprocess(admission_review).map_err(|e: Report| e.to_string())?;

    let tests = checking(&data, &obj, false)
        .await
        .map_err(|e: Report| e.to_string())?
        .into_iter()
        .all(|x| x);

    let tmp = ar.clone();
    let mut target_obj = obj.clone();
    let try_block = async || {
        if !tests {
            Err(eyre!(""))?;
        }

        for m in &data.matches {
            let Match {
                json_path,
                value: _,
                contains: _,
                value_to_be,
            } = &**m;
            if let Some(v) = value_to_be {
                let final_value = match v {
                    MatchValue::Value { value } => value.clone(),
                    MatchValue::JsonPath {
                        resource,
                        ignore_resource_not_exist: _,
                        json_path,
                    } => {
                        let ext_obj = if let Some(r) = resource {
                            Some(fetch_resource(r).await?.ok_or(eyre!(""))?) // value to be from ext resource cannot be "not found"
                        } else {
                            None
                        };
                        let ext_obj = ext_obj.as_ref().unwrap_or(&obj);
                        json_path.resolve(ext_obj)?.clone()
                    }
                };
                json_path.assign(&mut target_obj, final_value)?;
            }
        }

        let p = json_patch::diff(&obj, &target_obj);
        Ok(tmp.with_patch(p)?) as Result<_>
    };
    let ar = match try_block().await {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e:?}");
            ar.deny(format!("fail json path mutation on {}", data.name))
        }
    };
    review.response = Some(ar);
    review.request = None;
    Ok(Json(review))
}
