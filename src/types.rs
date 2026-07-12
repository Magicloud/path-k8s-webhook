use std::str::FromStr;

use evalexpr::{Node, build_operator_tree};
use eyre::{Report, Result, eyre};
use jsonptr::PointerBuf;
use serde_json::Value;

#[derive(Debug)]
pub struct Match {
    pub json_path: PointerBuf,
    pub value: Option<MatchValue>,
    pub contains: Contains,
    pub value_to_be: Option<MatchValue>,
}

#[derive(Debug, Clone)]
pub struct K8SResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
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

    ValueM(Value),
    ResourceM(K8SResource),
    PointerBufDM(PointerBuf),
}
