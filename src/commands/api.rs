//! `linear api` — make a raw GraphQL API request.
//!
//! Port of `src/commands/api.ts`. The command posts the caller's document and
//! variables to Linear and prints the response, choosing pretty JSON on a
//! terminal and compact JSON in a pipeline. `--paginate` follows a single
//! connection to exhaustion without the caller writing a loop.
//!
//! Reading stdin and the panic-free exits mirror upstream's `Deno.exit(1)`
//! paths: a 4xx body, a non-JSON body and a GraphQL error response all print
//! (unless `--silent`) and exit non-zero without the `✗` error wrapper.

use std::io::{IsTerminal, Read};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::consts;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct ApiArgs {
    /// GraphQL document. Use `-` to read from stdin.
    #[arg(value_name = "graphqlDocument")]
    pub document: Option<String>,

    /// Variable in key=value format (coerces booleans, numbers, null; @file reads from path)
    #[arg(long, value_name = "variable")]
    pub variable: Vec<String>,

    /// JSON object of variables (merged with --variable, which takes precedence)
    #[arg(long = "variables-json", value_name = "json")]
    pub variables_json: Option<String>,

    /// Auto-paginate a single connection field using cursor pagination
    #[arg(long)]
    pub paginate: bool,

    /// Suppress response output (exit code still reflects errors)
    #[arg(long)]
    pub silent: bool,
}

pub fn run(args: ApiArgs) -> Result<()> {
    // Upstream parses `--variable` with a custom cliffy type during argument
    // parsing, so a malformed entry fails *before* the action's try/catch and is
    // not wrapped in "API request failed". Keep that behaviour.
    for entry in &args.variable {
        if !entry.contains('=') {
            return Err(CliError::validation(format!(
                "Invalid variable format: {entry}. Variables must be in key=value format, \
                 e.g. --variable teamId=abc"
            )));
        }
    }

    run_inner(args).map_err(|error| error.with_context("API request failed"))
}

fn run_inner(args: ApiArgs) -> Result<()> {
    let query = resolve_query(args.document.as_deref())?;
    let variables = build_variables(&args.variable, args.variables_json.as_deref())?;

    let api_key = graphql::resolve_api_key_opt()?.ok_or_else(|| {
        CliError::validation("No API key configured").suggestion(
            "Set LINEAR_API_KEY, add api_key to .linear.toml, or run `linear auth login`.",
        )
    })?;

    let endpoint = graphql::endpoint();
    if args.paginate {
        execute_paginated(&endpoint, &api_key, &query, &variables, args.silent)
    } else {
        execute_single(&endpoint, &api_key, &query, &variables, args.silent)
    }
}

fn execute_single(
    endpoint: &str,
    api_key: &str,
    query: &str,
    variables: &Map<String, Value>,
    silent: bool,
) -> Result<()> {
    let mut body = Map::new();
    body.insert("query".to_string(), Value::String(query.to_string()));
    if !variables.is_empty() {
        body.insert("variables".to_string(), Value::Object(variables.clone()));
    }

    let (status, text) = post(endpoint, api_key, &Value::Object(body))?;

    if status >= 400 {
        if !silent {
            eprintln!("{text}");
        }
        std::process::exit(1);
    }

    let mut has_graphql_errors = false;
    match serde_json::from_str::<Value>(&text) {
        Ok(parsed) => {
            has_graphql_errors = parsed
                .get("errors")
                .and_then(Value::as_array)
                .map(|errors| !errors.is_empty())
                .unwrap_or(false);
            if !silent {
                output_json(&parsed, &text);
            }
        }
        Err(_) => {
            if !silent {
                output::line(&text);
            }
        }
    }

    if has_graphql_errors {
        std::process::exit(1);
    }
    Ok(())
}

fn execute_paginated(
    endpoint: &str,
    api_key: &str,
    query: &str,
    variables: &Map<String, Value>,
    silent: bool,
) -> Result<()> {
    let mut all_nodes: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut vars = variables.clone();
        vars.insert(
            "after".to_string(),
            cursor
                .as_ref()
                .map(|c| Value::String(c.clone()))
                .unwrap_or(Value::Null),
        );

        let mut body = Map::new();
        body.insert("query".to_string(), Value::String(query.to_string()));
        body.insert("variables".to_string(), Value::Object(vars));

        let (status, text) = post(endpoint, api_key, &Value::Object(body))?;
        if status >= 400 {
            if !silent {
                eprintln!("{text}");
            }
            std::process::exit(1);
        }

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(parsed) => parsed,
            Err(_) => {
                if !silent {
                    output::line(&text);
                }
                std::process::exit(1);
            }
        };

        if parsed
            .get("errors")
            .and_then(Value::as_array)
            .map(|errors| !errors.is_empty())
            .unwrap_or(false)
        {
            if !silent {
                output_json(&parsed, &text);
            }
            std::process::exit(1);
        }

        let data = parsed.get("data").cloned().unwrap_or(Value::Null);
        if all_nodes.is_empty() && count_connections(&data) > 1 {
            return Err(CliError::validation(
                "--paginate does not support queries with multiple paginated connections",
            )
            .suggestion(
                "Use cursor-based pagination manually with $after and pageInfo \
                 { hasNextPage endCursor }.",
            ));
        }

        let Some(page) = find_page_info(&parsed) else {
            if !silent {
                output_json(&parsed, &text);
            }
            return Ok(());
        };

        all_nodes.extend(page.nodes);

        if !page.has_next_page || page.end_cursor.is_none() {
            break;
        }
        cursor = page.end_cursor;
    }

    if !silent {
        let value = Value::Array(all_nodes);
        let compact = serde_json::to_string(&value).unwrap_or_else(|_| "[]".to_string());
        output_json(&value, &compact);
    }
    Ok(())
}

struct PageResult {
    nodes: Vec<Value>,
    has_next_page: bool,
    end_cursor: Option<String>,
}

fn find_page_info(value: &Value) -> Option<PageResult> {
    if let Some(array) = value.as_array() {
        for item in array {
            if let Some(result) = find_page_info(item) {
                return Some(result);
            }
        }
        return None;
    }

    let record = value.as_object()?;

    if record.contains_key("pageInfo") && record.contains_key("nodes") {
        if let Some(page_info) = record.get("pageInfo") {
            if page_info.is_object() {
                let nodes = record
                    .get("nodes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let has_next_page = page_info
                    .get("hasNextPage")
                    .map(js_truthy)
                    .unwrap_or(false);
                let end_cursor = page_info
                    .get("endCursor")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Some(PageResult {
                    nodes,
                    has_next_page,
                    end_cursor,
                });
            }
        }
    }

    for item in record.values() {
        if let Some(result) = find_page_info(item) {
            return Some(result);
        }
    }
    None
}

fn count_connections(value: &Value) -> usize {
    if value.is_null() {
        return 0;
    }
    if let Some(array) = value.as_array() {
        return array.iter().map(count_connections).sum();
    }
    let Some(record) = value.as_object() else {
        return 0;
    };
    if record.contains_key("pageInfo") && record.contains_key("nodes") {
        return 1;
    }
    record.values().map(count_connections).sum()
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().map(|n| n != 0.0).unwrap_or(true),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn output_json(parsed: &Value, raw_text: &str) {
    if std::io::stdout().is_terminal() {
        output::line(&output::to_pretty(parsed));
    } else {
        let text = match parsed {
            Value::String(_) => raw_text.to_string(),
            _ => serde_json::to_string(parsed).unwrap_or_else(|_| raw_text.to_string()),
        };
        output::raw(&text);
    }
}

fn resolve_query(positional: Option<&str>) -> Result<String> {
    if let Some(arg) = positional {
        if arg != "-" {
            return Ok(arg.to_string());
        }
    }

    let explicit = positional == Some("-");
    if explicit || !std::io::stdin().is_terminal() {
        let content = if explicit {
            read_all_stdin()
        } else {
            read_stdin_with_timeout(Duration::from_millis(100))
        };
        if let Some(content) = content {
            if !content.is_empty() {
                return Ok(content);
            }
        }
    }

    Err(CliError::validation("No query provided").suggestion(
        "Provide a query as an argument: linear api '{ viewer { id } }'\n  \
         Or pipe from stdin: echo '{ viewer { id } }' | linear api",
    ))
}

fn read_all_stdin() -> Option<String> {
    let mut buffer = String::new();
    if std::io::stdin().read_to_string(&mut buffer).is_err() {
        return None;
    }
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Mirror upstream's 100ms stdin race: only used when stdin is not a terminal
/// and no positional document was supplied.
fn read_stdin_with_timeout(timeout: Duration) -> Option<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_all_stdin());
    });
    receiver.recv_timeout(timeout).ok().flatten()
}

fn build_variables(
    entries: &[String],
    variables_json: Option<&str>,
) -> Result<Map<String, Value>> {
    let mut variables = Map::new();

    if let Some(raw) = variables_json {
        if !raw.is_empty() {
            let parsed: Value = serde_json::from_str(raw).map_err(|_| {
                CliError::validation(format!("Invalid JSON for --variables-json: {raw}"))
                    .suggestion(
                        "Provide a valid JSON object, e.g. --variables-json '{\"key\": \"value\"}'",
                    )
            })?;

            match parsed {
                Value::Object(map) => {
                    for (key, value) in map {
                        variables.insert(key, value);
                    }
                }
                other => {
                    let type_name = match other {
                        Value::Array(_) => "array",
                        Value::Null => "object",
                        Value::Bool(_) => "boolean",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                        Value::Object(_) => unreachable!(),
                    };
                    return Err(CliError::validation(format!(
                        "--variables-json must be a JSON object, got {type_name}"
                    ))
                    .suggestion(
                        "Provide a JSON object, e.g. --variables-json '{\"key\": \"value\"}'",
                    ));
                }
            }
        }
    }

    for entry in entries {
        if let Some((key, raw_value)) = entry.split_once('=') {
            variables.insert(key.to_string(), resolve_typed_value(raw_value)?);
        }
    }

    Ok(variables)
}

fn resolve_typed_value(value: &str) -> Result<Value> {
    if value == "@-" {
        return match read_all_stdin() {
            Some(content) => Ok(parse_json_or_string(&content)),
            None => Err(CliError::validation("No data on stdin for @- value")),
        };
    }

    if let Some(file_path) = value.strip_prefix('@') {
        return match std::fs::read_to_string(file_path) {
            Ok(content) => Ok(parse_json_or_string(content.trim())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(CliError::validation(format!("File not found: {file_path}")))
            }
            Err(error) => Err(
                CliError::cli(format!("Failed to read file: {file_path}")).cause(error)
            ),
        };
    }

    Ok(coerce_value(value))
}

fn parse_json_or_string(content: &str) -> Value {
    serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.to_string()))
}

/// JS `String(number)` for the values a `--variable` entry can round-trip.
/// Rust renders negative zero as "-0"; JS renders it as "0".
fn js_number_string(number: f64) -> String {
    if number == 0.0 {
        "0".to_string()
    } else {
        format!("{number}")
    }
}

fn coerce_value(value: &str) -> Value {
    match value {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        // JS `Number("Infinity")` round-trips (String(Infinity) === "Infinity"),
        // but JSON has no infinity, so it serializes as null.
        "Infinity" | "-Infinity" => return Value::Null,
        _ => {}
    }

    if !value.is_empty() {
        if let Ok(number) = value.parse::<f64>() {
            // Mirror JS `String(Number(value)) === value`: only coerce when the
            // canonical rendering round-trips, so "1.0", "1e3", "-0" stay strings.
            if number.is_finite() && js_number_string(number) == value {
                let integral = number.fract() == 0.0
                    && number >= -(2f64.powi(63))
                    && number < 2f64.powi(63);
                if integral {
                    return Value::Number(serde_json::Number::from(number as i64));
                }
                if let Some(json_number) = serde_json::Number::from_f64(number) {
                    return Value::Number(json_number);
                }
            }
        }
    }

    Value::String(value.to_string())
}

fn post(endpoint: &str, api_key: &str, body: &Value) -> Result<(u16, String)> {
    let user_agent = format!("{}/{}", consts::USER_AGENT_PREFIX, consts::VERSION);
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(graphql::REQUEST_TIMEOUT))
        .build()
        .new_agent();

    let mut response = agent
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", api_key)
        .header("User-Agent", &user_agent)
        .send_json(body)
        .map_err(|error| CliError::cli(format!("Failed to reach Linear API: {error}")))?;

    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| CliError::cli(format!("Failed to read Linear API response: {error}")))?;
    Ok((status, text))
}
