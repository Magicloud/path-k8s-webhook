use std::sync::Arc;

use axum::{Json, extract::State};
use evalexpr::{ContextWithMutableVariables, DefaultNumericTypes, HashMapContext};
use eyre::{Report, Result};
use kube::{api::DynamicObject, core::admission::AdmissionReview};
use serde_json::Value;

use crate::{
    helper::{checking, preprocess},
    types::MatchCombiner,
    webhook::Webhook,
};

pub async fn validate(
    State(data): State<Arc<Webhook>>,
    Json(admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    let (mut review, obj, mut ar) =
        preprocess(admission_review).map_err(|e: Report| e.to_string())?;

    let results = checking(&data, &obj, false)
        .await
        .map_err(|e: Report| e.to_string())?;
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
