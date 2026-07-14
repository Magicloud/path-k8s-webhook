use std::sync::Arc;

use axum::{Json, extract::State};
use eyre::{OptionExt, Result, eyre};
use kube::{api::DynamicObject, core::admission::AdmissionReview};
use serde_json::Value;

use crate::{
    helper::boilerplate,
    types::{Match, MatchValue},
    webhook::Webhook,
};

pub async fn mutate(
    State(data): State<Arc<Webhook>>,
    Json(admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    boilerplate(&data.name, admission_review, async |kind, obj, mut ar| {
        let tests = data
            .matches
            .checking(kind, obj, false)
            .await?
            .into_iter()
            .all(|x| x);

        if !tests {
            Err(eyre!("The incoming object failed testing"))?;
        }

        let mut target_obj = obj.clone();
        eprintln!("{:?}", data.matches.0);
        for m in &data.matches.0 {
            let Match {
                json_path,
                value: _,
                contains: _,
                value_to_be,
                kinds,
            } = &**m;
            if let Some(v) = value_to_be
                && kinds.iter().any(|x| x == kind)
            {
                let final_value = match v {
                    MatchValue::Value { value } => value.clone(),
                    MatchValue::JsonPath {
                        resource,
                        ignore_resource_not_exist: _,
                        json_path,
                    } => {
                        let ext_obj = if let Some(r) = resource {
                            Some(r.fetch().await?.ok_or_eyre(format!(
                                "{r:?} does not exist, cannot use value from it to mutate."
                            ))?)
                        } else {
                            None
                        };
                        let ext_obj = ext_obj.as_ref().unwrap_or(obj);
                        json_path.resolve(ext_obj)?.clone()
                    }
                };
                eprintln!("{final_value:?}");
                json_path.assign(&mut target_obj, final_value)?;
            }
        }

        let p = json_patch::diff(obj, &target_obj);
        eprintln!("{p:?}");
        ar = ar.with_patch(p)?;
        //                     ar.deny()
        Ok(ar)
    })
    .await
}
