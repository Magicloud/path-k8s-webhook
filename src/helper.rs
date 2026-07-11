use std::{collections::HashSet, sync::Arc};

use eyre::{Report, Result, eyre};
use futures::future::join_all;
use kube::{
    Api, Client, Discovery,
    api::{ApiResource, DynamicObject, GroupVersionKind},
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
};
use serde_json::Value;

use crate::{
    types::{Contains, K8SResource, Match, MatchValue},
    webhook::Webhook,
};

pub fn chunk_by<T, F>(vec: Vec<T>, mut predicate: F) -> Vec<Vec<T>>
where
    F: FnMut(&T) -> bool,
{
    let (mut result, last_chunk) =
        vec.into_iter()
            .fold((vec![], vec![]), |(mut result, mut chunk), i| {
                if !predicate(&i) && !chunk.is_empty() {
                    result.push(chunk);
                    chunk = vec![];
                }
                chunk.push(i);

                (result, chunk)
            });

    if !last_chunk.is_empty() {
        result.push(last_chunk);
    }

    result
}

pub async fn checking(
    data: &Arc<Webhook>,
    obj: &Value,
    ignore_containing: bool,
) -> Result<Vec<bool>> {
    let results = data
        .matches
        .iter()
        .map(async |m| match matching(m, obj, ignore_containing).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{e:}");
                false
            }
        })
        .collect::<Vec<_>>();
    let results: Vec<_> = join_all(results).await;
    Ok(results)
}

pub fn preprocess(
    mut admission_review: Value,
) -> std::prelude::v1::Result<(AdmissionReview<DynamicObject>, Value, AdmissionResponse), Report> {
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
    let ar = AdmissionResponse::from(&serde_json::from_value::<AdmissionRequest<DynamicObject>>(
        req,
    )?);
    Ok((review, obj, ar))
}

async fn matching(m: &Match, obj: &Value, ignore_containing: bool) -> Result<bool> {
    let results = m.json_path.resolve(obj)?;

    let check = |target_values: &Value| {
        let result = (ignore_containing && target_values == results)
            || match m.contains {
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
                Some(fetch_resource(r).await?)
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

pub async fn fetch_resource(r: &K8SResource) -> Result<Option<Value>> {
    let client = Client::try_default().await?;
    // run_aggregated() does not work with K3S.
    let dis = Discovery::new(client.clone()).run().await?;
    let gvk = dis
        .groups()
        .find_map(|g| {
            g.resources_by_stability()
                .iter()
                .find(|(ar, _)| ar.kind.eq_ignore_ascii_case(&r.kind))
                .map(|(ar, _)| GroupVersionKind::gvk(g.name(), &ar.version, &ar.kind))
        })
        .ok_or(eyre!(""))?;
    eprintln!("{gvk:?}");
    let ar = ApiResource::from_gvk(&gvk);
    let api: Api<DynamicObject> = if let Some(ref ns) = r.namespace {
        Api::namespaced_with(client, ns, &ar)
    } else {
        Api::default_namespaced_with(client, &ar)
    };
    let dyn_obj = api.get_opt(&r.name).await?;
    let target_obj = dyn_obj.map(serde_json::to_value).transpose()?;
    Ok(target_obj)
}
