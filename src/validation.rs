use std::{error::Error, sync::Arc};

use evalexpr::{ContextWithMutableVariables, DefaultNumericTypes, HashMapContext};
use eyre::{Result, eyre};
use kube::{api::DynamicObject, core::admission::AdmissionReview};
use serde_json::Value;
use tracing::warn;
use xitca_web::{
    codegen::route,
    handler::{json::Json, state::StateRef},
};

use crate::{helper::boilerplate, types::MatchCombiner, webhook::Webhook};

#[route("/validate", method = post)]
pub async fn validate(
    data: StateRef<'_, Arc<Webhook>>,
    Json(admission_review): Json<Value>,
) -> Result<Json<AdmissionReview<DynamicObject>>, Box<dyn Error + Send + Sync>> {
    let ret = boilerplate(&data.name, admission_review, async |kind, obj, ar| {
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
                try_block().unwrap_or_else(|e| {
                    warn!("{e}");
                    false
                })
            }
        };

        if result {
            Ok(ar)
        } else {
            Err(eyre!("Validate failed"))
        }
    })
    .await?;
    Ok(ret)
}
