use std::{collections::HashSet, str::FromStr, sync::Arc};

use clap::ArgMatches;
use evalexpr::{Node, build_operator_tree};
use eyre::{OptionExt, Report, Result, eyre};
use futures::future::join_all;
use jsonptr::PointerBuf;
use kube::{
    Api, Client, Discovery,
    api::{ApiResource, DynamicObject, GroupVersionKind},
};
use macroweave::repeat;
use serde_json::Value;
use tracing::{debug, warn};

pub struct Matches(pub Vec<Arc<Match>>);
impl Matches {
    pub async fn checking(
        &self,
        kind: &str,
        obj: &Value,
        ignore_containing: bool,
    ) -> Result<Vec<bool>> {
        let results = self
            .0
            .iter()
            .map(
                async |m| match m.matching(kind, obj, ignore_containing).await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("{e:}");
                        false
                    }
                },
            )
            .collect::<Vec<_>>();
        let results: Vec<_> = join_all(results).await;
        Ok(results)
    }
}
impl TryFrom<&mut ArgMatches> for Matches {
    type Error = Report;

    fn try_from(value: &mut ArgMatches) -> Result<Self> {
        repeat!((Key, Wrapper) in [
            (json_path, TypeHelper::PointerBufS),
            (jp_kinds, TypeHelper::StringK),
            (jp_value, TypeHelper::Value),
            (jp_ignore, TypeHelper::BoolI),
            (jp_resource, TypeHelper::Resource),
            (jp_value_json_path, TypeHelper::PointerBufD),
            (jp_contains, TypeHelper::Contains),

            (jp_value_m, TypeHelper::ValueM),
            (jp_resource_m, TypeHelper::ResourceM),
            (jp_value_m_json_path, TypeHelper::PointerBufDM),
        ] {
            let Key = (|key| {
                let i = value.indices_of(key)?.collect::<Vec<_>>();
                let k = value.remove_many(key)?;
                Some(k.map(Wrapper).zip(i).collect::<Vec<_>>())
            })(stringify!(Key))
            .unwrap_or_default();
        });
        let mut prepare = [
            json_path,
            jp_kinds,
            jp_value,
            jp_ignore,
            jp_resource,
            jp_value_json_path,
            jp_contains,
            jp_value_m,
            jp_resource_m,
            jp_value_m_json_path,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        prepare.sort_by_key(|(_, i)| *i);
        eprintln!("{prepare:?}");
        let matches = prepare
            .into_iter()
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
            .chunk_by_value(|n| !matches!(n, TypeHelper::PointerBufS(_)))
            .into_iter()
            .map(|match_data| {
                let m = Match::try_from(match_data)?;
                Ok(m.into())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self(matches))
    }
}

#[derive(Debug)]
pub struct Match {
    pub json_path: PointerBuf,
    pub kinds: Vec<String>,
    pub value: Option<MatchValue>,
    pub contains: Contains,
    pub value_to_be: Option<MatchValue>,
}
impl Match {
    pub async fn matching(&self, kind: &str, obj: &Value, ignore_containing: bool) -> Result<bool> {
        // `contains` requires alloc for kind, as the types are not the same.
        if !self.kinds.iter().any(|x| x == kind) {
            return Ok(false);
        }

        let md = obj.get("metadata").unwrap_or_default();
        let n = md.get("name").unwrap_or_default();
        let ns = md.get("namespace").unwrap_or_default();
        debug!(
            "Matching {kind}:{ns}/{n}:{} to {:?}",
            self.json_path, self.value
        );

        let results = self.json_path.resolve(obj)?;

        debug!("Current value: {results:?}");

        let check = |target_values: &Value| {
            let result = if ignore_containing {
                target_values == results
            } else {
                match self.contains {
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
                }
            };
            Ok(result) as Result<_>
        };

        let result = match &self.value {
            Some(MatchValue::JsonPath {
                json_path,
                resource,
                ignore_resource_not_exist,
            }) => {
                let ext_obj = if let Some(r) = resource {
                    Some((r, r.fetch().await?))
                } else {
                    None
                };
                match &ext_obj {
                    Some((_, None)) if *ignore_resource_not_exist => true, // resource required, but not exist and ignore.
                    Some((r, None)) => Err(eyre!("{r:?} does not exist"))?, // resource required, but not exist.
                    Some((_, Some(ext_obj))) => {
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

        debug!("Match result: {result}");
        Ok(result)
    }
}
impl TryFrom<Vec<TypeHelper>> for Match {
    type Error = Report;

    fn try_from(input_value: Vec<TypeHelper>) -> Result<Self> {
        let mut query = None;
        let mut kinds = vec![];
        let mut contains = Contains::Equal;
        let mut value = None;
        let mut resource = None;
        let mut ignore = false;
        let mut target_jp = None;

        let mut value_m = None;
        let mut resource_m = None;
        let mut target_jp_m = None;
        for i in input_value {
            match i {
                TypeHelper::PointerBufS(jp_query) => query = Some(jp_query),
                TypeHelper::PointerBufD(jp_query) => target_jp = Some(jp_query),
                TypeHelper::Value(v) => value = Some(v),
                TypeHelper::Resource(r) => resource = Some(r),
                TypeHelper::BoolI(b) => ignore = b,
                TypeHelper::Contains(c) => contains = c,
                TypeHelper::ValueM(v) => value_m = Some(v),
                TypeHelper::ResourceM(r) => resource_m = Some(r),
                TypeHelper::PointerBufDM(p) => target_jp_m = Some(p),
                TypeHelper::StringK(k) => kinds.push(k),
            }
        }

        let query = query.ok_or_eyre("There must be a json path to start")?;
        let mut m = Self {
            json_path: query,
            value: None,
            contains,
            value_to_be: None,
            kinds,
        };
        match (value, target_jp) {
            (None, None) => (),
            (None, Some(v)) => {
                m.value = Some(MatchValue::JsonPath {
                    json_path: v,
                    resource,
                    ignore_resource_not_exist: ignore,
                });
            }
            (Some(v), None) => m.value = Some(MatchValue::Value { value: v }),
            (Some(_), Some(_)) => {
                Err(eyre!("-v and -p cannot be used together"))?;
            }
        }
        match (value_m, target_jp_m) {
            (None, None) => (),
            (None, Some(v)) => {
                m.value_to_be = Some(MatchValue::JsonPath {
                    resource: resource_m,
                    ignore_resource_not_exist: false,
                    json_path: v,
                });
            }
            (Some(v), None) => m.value_to_be = Some(MatchValue::Value { value: v }),
            (Some(_), Some(_)) => {
                Err(eyre!(
                    "--jp-value-m and --jp-value-m-json-path cannot be used together"
                ))?;
            }
        }
        Ok(m)
    }
}

#[derive(Debug, Clone)]
pub struct K8SResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}
impl K8SResource {
    pub async fn fetch(&self) -> Result<Option<Value>> {
        let client = Client::try_default().await?;
        // run_aggregated() does not work with K3S.
        let dis = Discovery::new(client.clone()).run().await?;
        let gvk = dis
            .groups()
            .find_map(|g| {
                g.resources_by_stability()
                    .iter()
                    .find(|(ar, _)| ar.kind.eq_ignore_ascii_case(&self.kind))
                    .map(|(ar, _)| GroupVersionKind::gvk(g.name(), &ar.version, &ar.kind))
            })
            .ok_or_eyre(format!("{} does not exist", self.kind))?;
        debug!("{gvk:?}");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = if let Some(ref ns) = self.namespace {
            Api::namespaced_with(client, ns, &ar)
        } else {
            Api::default_namespaced_with(client, &ar)
        };
        let dyn_obj = api.get_opt(&self.name).await?;
        let target_obj = dyn_obj.map(serde_json::to_value).transpose()?;
        if target_obj.is_none() {
            debug!("Object {:?}/{} does not exist", self.namespace, self.name);
        }
        Ok(target_obj)
    }
}
impl FromStr for K8SResource {
    type Err = Report;

    fn from_str(s: &str) -> Result<Self> {
        if let Some((k, nsn)) = s.split_once(':') {
            if let Some((ns, n)) = nsn.split_once('/') {
                Ok(Self {
                    kind: k.to_string(),
                    namespace: Some(ns.to_string()),
                    name: n.to_string(),
                })
            } else {
                Ok(Self {
                    kind: k.to_string(),
                    namespace: None,
                    name: nsn.to_string(),
                })
            }
        } else {
            Err(eyre!("Cannot parse resource, missing `:`."))
        }
    }
}

#[derive(Debug)]
pub enum MatchValue {
    Value {
        value: Value,
    },
    JsonPath {
        resource: Option<K8SResource>,
        ignore_resource_not_exist: bool,
        json_path: PointerBuf,
    },
}

#[derive(Debug, Clone)]
pub enum MatchCombiner {
    Any,
    All,
    BooleanExpression(Node),
}
impl FromStr for MatchCombiner {
    type Err = Report;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_uppercase().as_str() {
            "" | "ALL" => Ok(Self::All),
            "ANY" => Ok(Self::Any),
            o => Ok(Self::BooleanExpression(build_operator_tree(o)?)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Contains {
    Equal,
    Intersect,
    Contain,
}
impl FromStr for Contains {
    type Err = Report;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_uppercase().as_str() {
            "" | "EQUAL" => Ok(Self::Equal),
            "CONTAIN" => Ok(Self::Contain),
            "INTERSECT" => Ok(Self::Intersect),
            _ => unimplemented!(),
        }
    }
}

#[derive(Debug)]
pub enum TypeHelper {
    PointerBufS(PointerBuf),
    PointerBufD(PointerBuf),
    Value(Value),
    Resource(K8SResource),
    Contains(Contains),
    BoolI(bool),
    StringK(String),

    ValueM(Value),
    ResourceM(K8SResource),
    PointerBufDM(PointerBuf),
}

pub trait VecExt<T>: IntoIterator<Item = T> {
    fn chunk_by_value<F>(self, mut predicate: F) -> Vec<Vec<T>>
    where
        F: FnMut(&T) -> bool,
        Self: Sized,
    {
        let (mut result, last_chunk) =
            self.into_iter()
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
}
impl<T> VecExt<T> for Vec<T> {}
