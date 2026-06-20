use std::path::PathBuf;

use clap::{
    builder::{StringValueParser, TypedValueParser},
    *,
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
    #[arg(short, long, value_parser = StringValueParser::new().try_map(|s| parse_json_path(&s)))]
    pub json_path: JpQuery,
    #[arg(short, long)]
    pub value: Option<String>,
    #[arg(short, long, default_value = "true", requires = "value")]
    pub all_must_match: bool,
    /// Webhook service TLS certificate file path
    #[arg(short('c'), long)]
    pub tls_certificate_file_name: PathBuf,
    /// Webhook service TLS private key file path
    #[arg(short('k'), long)]
    pub tls_private_key_file_name: PathBuf,
}
