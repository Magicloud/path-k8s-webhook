use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{OptionExt, Result};
use kube::{
    Client, Discovery,
    api::{DynamicObject, GroupVersionKind},
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;
use tracing::{info, instrument};
use xitca_web::handler::json::Json;

use crate::DISCOVERY_CACHE;

#[instrument(skip_all)]
pub async fn boilerplate<F>(
    name: &str,
    mut admission_review: Value,
    f: F,
) -> Result<Json<AdmissionReview<DynamicObject>>>
where
    F: AsyncFnOnce(&String, &Value, AdmissionResponse) -> Result<AdmissionResponse>,
{
    let mut review =
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
    let mut ar = AdmissionResponse::from(
        &serde_json::from_value::<AdmissionRequest<DynamicObject>>(req)?,
    );
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

#[instrument(skip_all)]
pub async fn kind_string_to_gvk(kind: &str, client: Client) -> Result<GroupVersionKind> {
    let mut ts_d = DISCOVERY_CACHE.load();
    let ts = ts_d.0;
    if ts.elapsed() > Duration::from_hours(1) {
        // update
        // run_aggregated() does not work with K3S.
        let d = Discovery::new(client.clone()).run().await?;
        DISCOVERY_CACHE.store(Arc::new((Instant::now(), Some(d))));
        ts_d = DISCOVERY_CACHE.load();
    }
    let dis = ts_d.1.as_ref().unwrap();

    let gvk = dis.groups().find_map(|g| {
        g.resources_by_stability()
            .iter()
            .find(|(ar, _)| ar.kind.eq_ignore_ascii_case(kind))
            .map(|(ar, _)| GroupVersionKind::gvk(g.name(), &ar.version, &ar.kind))
    });
    if let Some(gvk) = gvk {
        Ok(gvk)
    } else {
        // try again
        let d = Discovery::new(client).run().await?;
        DISCOVERY_CACHE.store(Arc::new((Instant::now(), Some(d))));
        let ts_d = DISCOVERY_CACHE.load();
        let dis = ts_d.1.as_ref().unwrap();

        let gvk = dis
            .groups()
            .find_map(|g| {
                g.resources_by_stability()
                    .iter()
                    .find(|(ar, _)| ar.kind.eq_ignore_ascii_case(kind))
                    .map(|(ar, _)| GroupVersionKind::gvk(g.name(), &ar.version, &ar.kind))
            })
            .ok_or_eyre(format!("{kind} does not exist"))?;
        Ok(gvk)
    }
}
