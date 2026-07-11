use std::path::PathBuf;

use clap::{
    ArgAction::{Append, SetTrue},
    Args, Parser, Subcommand,
    builder::{StringValueParser, TypedValueParser},
    value_parser,
};
use jsonptr::PointerBuf;
use serde_json::Value;

use crate::types::{Contains, K8SResource, MatchCombiner};

#[derive(Subcommand, Debug)]
#[command(rename_all = "lower")]
pub enum SubCmd {
    Webhook(WebhookArguments),
}

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: SubCmd,
}

#[derive(Debug, Args)]
pub struct WebhookArguments {
    /// The JSON path to match.
    /// If it intends to return values, `-v` and `-o` could be used to check the values.
    /// `-o` means containing, means query result or value, one contains another.
    /// Or, do not pass `-v` and `-o` just to check if there are values (existence).
    /// If the value should be fetched via another JSON path, use `-p`. `-v` and `-p` cannot be appear together.
    /// When both query result and value (or result from `-p`) are vecs. There are four cases.
    /// 1. The two sets equal. This is default by not specifying `-o`.
    /// 2. The two sets intersect. Use `-o INTERSECT`.
    /// 3. and 4. One set includes the other. (Subsume if expressing the backward). Covered by `-o`
    ///
    /// Additionally, `-r` can be used with `-p`, to specify a K8S resource to query for values to compare. The format of `-r` value is "Kind:Namespace/Name". And `-i` to skip validation if the resource does not exist.
    #[arg(short('j'), long, required = true, action = Append, value_parser = StringValueParser::new().try_map(|s| PointerBuf::parse(&s)))]
    pub json_path: Vec<PointerBuf>,
    #[arg(short('v'), long, action = Append, value_parser = StringValueParser::new().try_map(|s| serde_json::from_str::<Value>(&s)))]
    pub jp_value: Vec<Value>,
    #[arg(short('i'), long, action = SetTrue)]
    pub jp_ignore: Vec<bool>,
    #[arg(short('r'), long, action = Append, value_parser = value_parser!(K8SResource))]
    pub jp_resource: Vec<K8SResource>,
    #[arg(short('p'), long, action = Append, value_parser = StringValueParser::new().try_map(|s| PointerBuf::parse(&s)))]
    pub jp_value_json_path: Vec<PointerBuf>,
    #[arg(short('o'), long, action = Append, num_args = 0..=1, default_value = "EQUAL", default_missing_value = "CONTAIN", value_parser = value_parser!(Contains))]
    pub jp_contains: Vec<Contains>,

    /// Mutation options, mirrors of validation.
    /// A Value, or a query to get value, or a resource and a query to get value, must be specified.
    /// If both validation and mutation values are specified, it means test first.
    #[arg(long, action = Append, value_parser = StringValueParser::new().try_map(|s| serde_json::from_str::<Value>(&s)))]
    pub jp_value_m: Vec<Value>,
    #[arg(long, action = Append)]
    pub jp_resource_m: Vec<String>,
    #[arg(long, action = Append, value_parser = StringValueParser::new().try_map(|s| PointerBuf::parse(&s)))]
    pub jp_value_m_json_path: Vec<PointerBuf>,

    /// How to combine the results of all matches specified by `-j`, `-v`, `-p`.
    /// Without this option, the results are combined by **any**.
    /// With this option, but not giving a value, the results are combined by **all**.
    /// With this option, and giving a _boolean\_expression_, the results are combined by the expression.
    #[arg(short('A'), long, num_args = 0..=1, default_value = "ANY", default_missing_value = "ALL", value_parser = value_parser!(MatchCombiner))]
    pub match_combiner: Option<String>,

    /// Webhook service TLS certificate file path
    #[arg(short('c'), long)]
    pub tls_certificate_file_name: PathBuf,
    /// Webhook service TLS private key file path
    #[arg(short('k'), long)]
    pub tls_private_key_file_name: PathBuf,
    #[arg(short('n'), long)]
    pub name: String,
}
