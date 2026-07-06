use std::path::PathBuf;

use clap::{
    ArgAction::{Append, SetTrue},
    Args, Parser, Subcommand,
    builder::{StringValueParser, TypedValueParser},
};
use jsonpath_rust::parser::{model::JpQuery, parse_json_path};

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
    /// Giving how JSON path works, the result could be some values, or nothing.
    /// If it intends to return values, `-v` and `-a` could be used to check the values.
    /// Or, do not pass `-v` and `-a` just to check if there are values (existence).
    ///  If the value should be fetched via another JSON path, use `-p`. `-v` and `-p` cannot be appear together. Gving both paths returns a set of results, there would be four cases.
    /// 1. The two sets equal.
    /// 2. Thw two sets intersect.
    /// 3. and 4. One set includes the other. (Subsume if expressing the backward). These two cases are not implenmented yet.
    #[arg(short('j'), long, required = true, action = Append, value_parser = StringValueParser::new().try_map(|s| parse_json_path(&s)))]
    pub json_path: Vec<JpQuery>,
    #[arg(short('v'), long, action = Append)]
    pub jp_value: Vec<String>,
    #[arg(short('p'), long, action = Append, value_parser = StringValueParser::new().try_map(|s| parse_json_path(&s)))]
    pub jp_value_json_path: Vec<JpQuery>,
    #[arg(short('a'), long, action = SetTrue)]
    pub jp_all_must_match: Vec<bool>,

    /// How to combine the results of all matches specified by `-j`, `-v`, `-a`.
    /// Without this option, the results are combined by **any**.
    /// With this option, but not giving a value, the results are combined by **all**.
    /// With this option, and giving a _boolean\_expression_, the results are combined by the expression.
    #[arg(short('A'), long, num_args = 0..=1, default_missing_value = "ALL")]
    pub match_combiner: Option<String>,

    /// Webhook service TLS certificate file path
    #[arg(short('c'), long)]
    pub tls_certificate_file_name: PathBuf,
    /// Webhook service TLS private key file path
    #[arg(short('k'), long)]
    pub tls_private_key_file_name: PathBuf,
    #[arg(short, long)]
    pub name: String,
}

#[derive(Debug)]
pub enum TypeHelper {
    JpQueryS(JpQuery),
    JpQueryD(JpQuery),
    String(String),
    Bool(bool),
}
