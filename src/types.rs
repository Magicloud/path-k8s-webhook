use std::{path::PathBuf, sync::Arc};

use clap::ArgMatches;
use evalexpr::{Node, build_operator_tree};
use eyre::{Report, Result, eyre};
use jsonpath_rust::parser::model::JpQuery;

use crate::helper::chunk_by;

#[derive(Debug)]
pub struct Match {
    pub json_path: JpQuery,
    pub value: Option<MatchValue>,
}

#[derive(Debug)]
pub struct K8SResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}
impl TryFrom<String> for K8SResource {
    type Error = Report;

    fn try_from(value: String) -> Result<Self> {
        if let Some((k, nsn)) = value.split_once(':') {
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
        value: String,
        all_must_match: bool,
    },
    JsonPath {
        resource: Option<K8SResource>,
        ignore_resource_not_exist: bool,
        json_path: JpQuery,
        all_must_match: bool,
    },
}

#[derive(Debug)]
pub enum MatchCombiner {
    Any,
    All,
    BooleanExpression(Node),
}

pub struct Webhook {
    pub matches: Vec<Arc<Match>>,
    pub combiner: MatchCombiner,
    pub tls_certificate_file_name: PathBuf,
    pub tls_private_key_file_name: PathBuf,
    pub name: String,
}
impl TryFrom<ArgMatches> for Webhook {
    type Error = eyre::Report;

    fn try_from(mut args: ArgMatches) -> Result<Self> {
        let json_path = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::JpQueryS).zip(i).collect::<Vec<_>>())
        })("json_path")
        .unwrap_or_default();

        let value = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::StringV).zip(i).collect::<Vec<_>>())
        })("jp_value")
        .unwrap_or_default();

        let ignore = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::BoolI).zip(i).collect::<Vec<_>>())
        })("jp_ignore")
        .unwrap_or_default();

        let resource = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::StringR).zip(i).collect::<Vec<_>>())
        })("jp_resource")
        .unwrap_or_default();

        let value_json_path = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::JpQueryD).zip(i).collect::<Vec<_>>())
        })("jp_value_json_path")
        .unwrap_or_default();

        let all_must_match = (|key| {
            let i = args.indices_of(key)?.collect::<Vec<_>>();
            let k = args.remove_many(key)?;
            Some(k.map(TypeHelper::BoolA).zip(i).collect::<Vec<_>>())
        })("jp_all_must_match")
        .unwrap_or_default();

        let mut prepare = [
            json_path,
            value,
            ignore,
            resource,
            value_json_path,
            all_must_match,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        prepare.sort_by_key(|(_, i)| *i);
        let matches = chunk_by(prepare, |(n, _)| !matches!(n, TypeHelper::JpQueryS(_)))
            .into_iter()
            .map(|match_data| {
                let mut query = None;
                let mut all = false;
                let mut value = None;
                let mut resource = None;
                let mut ignore = false;
                let mut target_jp = None;
                for (i, _) in match_data {
                    match i {
                        TypeHelper::JpQueryS(jp_query) => query = Some(jp_query),
                        TypeHelper::JpQueryD(jp_query) => target_jp = Some(jp_query),
                        TypeHelper::StringV(s) => value = Some(s),
                        TypeHelper::BoolA(b) => all = b,
                        TypeHelper::StringR(s) => resource = Some(K8SResource::try_from(s)?),
                        TypeHelper::BoolI(b) => ignore = b,
                    }
                }

                let query = query.ok_or_else(|| eyre!(""))?;
                match (value, target_jp) {
                    (None, None) => Ok(Match {
                        json_path: query,
                        value: None,
                    }
                    .into()),
                    (None, Some(v)) => Ok(Match {
                        json_path: query,
                        value: Some(MatchValue::JsonPath {
                            json_path: v,
                            all_must_match: all,
                            resource,
                            ignore_resource_not_exist: ignore,
                        }),
                    }
                    .into()),
                    (Some(v), None) => Ok(Match {
                        json_path: query,
                        value: Some(MatchValue::Value {
                            value: v,
                            all_must_match: all,
                        }),
                    }
                    .into()),
                    (Some(_), Some(_)) => Err(eyre!("-v and -p cannot be used together")),
                }
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            matches,
            combiner: match args.remove_one("match_combiner") {
                None => MatchCombiner::Any,
                Some(str) => {
                    if str == "ALL" {
                        MatchCombiner::All
                    } else {
                        MatchCombiner::BooleanExpression(build_operator_tree(str)?)
                    }
                }
            },
            tls_certificate_file_name: args
                .remove_one("tls_certificate_file_name")
                .ok_or_else(|| eyre!("No tls_certificate_file_name specified"))?,
            tls_private_key_file_name: args
                .remove_one("tls_private_key_file_name")
                .ok_or_else(|| eyre!("No tls_private_key_file_name specified"))?,
            name: args
                .remove_one("name")
                .ok_or_else(|| eyre!("No name specified"))?,
        })
    }
}

#[derive(Debug)]
pub enum TypeHelper {
    JpQueryS(JpQuery),
    JpQueryD(JpQuery),
    StringV(String),
    StringR(String),
    BoolI(bool),
    BoolA(bool),
}
