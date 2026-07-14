use std::sync::Arc;

use axum::{Json, extract::State};
use evalexpr::{ContextWithMutableVariables, DefaultNumericTypes, HashMapContext};
use eyre::{Result, eyre};
use kube::{api::DynamicObject, core::admission::AdmissionReview};
use serde_json::Value;

use crate::{helper::boilerplate, types::MatchCombiner, webhook::Webhook};

pub async fn validate(
    State(data): State<Arc<Webhook>>,
    Json(admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, String> {
    boilerplate(&data.name, admission_review, async |kind, obj, ar| {
        let results = data.matches.checking(kind, obj, false).await?;
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
            Ok(ar)
        } else {
            Err(eyre!("Validate failed"))
        }
    })
    .await
}
