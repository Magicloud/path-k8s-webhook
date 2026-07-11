use evalexpr::Node;
use eyre::{Report, Result, eyre};
use jsonptr::PointerBuf;
use serde_json::Value;

#[derive(Debug)]
pub struct Match {
    pub json_path: PointerBuf,
    pub value: Option<MatchValue>,
    pub contains: Contains,
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
        value: Value,
    },
    JsonPath {
        resource: Option<K8SResource>,
        ignore_resource_not_exist: bool,
        json_path: PointerBuf,
    },
}

#[derive(Debug)]
pub enum MatchCombiner {
    Any,
    All,
    BooleanExpression(Node),
}

#[derive(Debug)]
pub enum Contains {
    Equal,
    Intersect,
    Contain,
}

#[derive(Debug)]
pub enum TypeHelper {
    PointerBufS(PointerBuf),
    PointerBufD(PointerBuf),
    Value(Value),
    StringR(String),
    StringC(String),
    BoolI(bool),

    ValueM(Value),
    StringRM(String),
    PointerBufDM(PointerBuf),
}
