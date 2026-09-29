//! `linear schema` — print the GraphQL schema.
//!
//! Port of `src/commands/schema.ts`. Upstream fetches `getIntrospectionQuery()`
//! and then runs `buildClientSchema` → `lexicographicSortSchema` → `printSchema`
//! from graphql-js to emit SDL. This port fetches the same introspection
//! document and reproduces those three transformations over the raw JSON, so
//! the SDL matches graphql-js byte for byte: types/fields/args/enum values are
//! natural-sorted, specified scalars and `__*` introspection types are
//! omitted, and descriptions are printed as GraphQL block strings.
//!
//! `--json` bypasses SDL and pretty-prints the introspection result; `-o/--output`
//! writes either form to a file, appending exactly one newline.

use std::cmp::Ordering;

use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::graphql;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct SchemaArgs {
    /// Output as JSON introspection result instead of SDL
    #[arg(long)]
    pub json: bool,
    /// Write schema to file instead of stdout
    #[arg(short = 'o', long, value_name = "file")]
    pub output: Option<String>,
}

/// The default `getIntrospectionQuery()` document (descriptions on; no
/// specifiedByUrl, repeatable directives, schema description, input deprecation
/// or oneOf). The `TypeRef` fragment nests `ofType` as deeply as graphql-js
/// emits so wrapped list/non-null types print exactly.
const INTROSPECTION_QUERY: &str = r#"query IntrospectionQuery {
  __schema {
    queryType { name kind }
    mutationType { name kind }
    subscriptionType { name kind }
    types { ...FullType }
    directives {
      name
      description
      locations
      args { ...InputValue }
    }
  }
}

fragment FullType on __Type {
  kind
  name
  description
  fields(includeDeprecated: true) {
    name
    description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated
    deprecationReason
  }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) {
    name
    description
    isDeprecated
    deprecationReason
  }
  possibleTypes { ...TypeRef }
}

fragment InputValue on __InputValue {
  name
  description
  type { ...TypeRef }
  defaultValue
}

fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType {
              kind
              name
              ofType {
                kind
                name
                ofType {
                  kind
                  name
                  ofType {
                    kind
                    name
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}"#;

pub fn run(args: SchemaArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to fetch schema"))
}

fn run_inner(args: SchemaArgs) -> Result<()> {
    let client = graphql::client()?;
    let data = client.request(INTROSPECTION_QUERY, json!({}))?;

    let schema = data.get("__schema").ok_or_else(|| {
        CliError::cli("Introspection response did not contain a __schema field")
    })?;

    let content = if args.json {
        output::to_pretty(&data)
    } else {
        print_schema(schema)
    };

    match &args.output {
        Some(path) => {
            std::fs::write(path, format!("{content}\n")).map_err(|error| {
                CliError::cli(format!("Failed to write {path}")).cause(error)
            })?;
            output::line(&format!("Schema written to {path}"));
        }
        None => output::line(&content),
    }
    Ok(())
}

// --- SDL printing (ports graphql-js buildClientSchema + lexicographicSortSchema
// --- + printSchema, operating on introspection JSON directly) ---

fn name_of(value: &Value) -> String {
    value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn sorted_by_name(items: &[Value]) -> Vec<&Value> {
    let mut sorted: Vec<&Value> = items.iter().collect();
    sorted.sort_by(|a, b| natural_compare(&name_of(a), &name_of(b)));
    sorted
}

fn print_schema(schema: &Value) -> String {
    let query_name = schema
        .pointer("/queryType/name")
        .and_then(Value::as_str);
    let mutation_name = schema
        .pointer("/mutationType/name")
        .and_then(Value::as_str);
    let subscription_name = schema
        .pointer("/subscriptionType/name")
        .and_then(Value::as_str);

    // `schemaDescription` is not requested, so only a non-conventional root
    // naming forces an explicit `schema { ... }` block.
    let common_names = query_name.map_or(true, |n| n == "Query")
        && mutation_name.map_or(true, |n| n == "Mutation")
        && subscription_name.map_or(true, |n| n == "Subscription");

    let mut parts: Vec<String> = Vec::new();

    if !common_names {
        let mut operation_types = Vec::new();
        if let Some(name) = query_name {
            operation_types.push(format!("  query: {name}"));
        }
        if let Some(name) = mutation_name {
            operation_types.push(format!("  mutation: {name}"));
        }
        if let Some(name) = subscription_name {
            operation_types.push(format!("  subscription: {name}"));
        }
        parts.push(format!("schema {{\n{}\n}}", operation_types.join("\n")));
    }

    if let Some(directives) = schema.get("directives").and_then(Value::as_array) {
        let custom: Vec<&Value> = directives
            .iter()
            .filter(|directive| !is_specified_directive(&name_of(directive)))
            .collect();
        for directive in sorted_by_name_refs(&custom) {
            parts.push(print_directive(directive));
        }
    }

    if let Some(types) = schema.get("types").and_then(Value::as_array) {
        let defined: Vec<&Value> = types.iter().filter(|ty| is_defined_type(ty)).collect();
        for ty in sorted_by_name_refs(&defined) {
            parts.push(print_type(ty));
        }
    }

    parts.retain(|part| !part.is_empty());
    parts.join("\n\n")
}

fn sorted_by_name_refs<'a>(items: &[&'a Value]) -> Vec<&'a Value> {
    let mut sorted: Vec<&Value> = items.to_vec();
    sorted.sort_by(|a, b| natural_compare(&name_of(a), &name_of(b)));
    sorted
}

fn is_specified_directive(name: &str) -> bool {
    matches!(
        name,
        "include" | "skip" | "deprecated" | "specifiedBy" | "oneOf"
    )
}

fn is_defined_type(ty: &Value) -> bool {
    let name = name_of(ty);
    if name.starts_with("__") {
        return false;
    }
    if ty.get("kind").and_then(Value::as_str) == Some("SCALAR")
        && matches!(name.as_str(), "Int" | "Float" | "String" | "Boolean" | "ID")
    {
        return false;
    }
    true
}

fn print_type(ty: &Value) -> String {
    match ty.get("kind").and_then(Value::as_str).unwrap_or("") {
        "SCALAR" => print_scalar(ty),
        "OBJECT" => print_object(ty),
        "INTERFACE" => print_interface(ty),
        "UNION" => print_union(ty),
        "ENUM" => print_enum(ty),
        "INPUT_OBJECT" => print_input_object(ty),
        _ => String::new(),
    }
}

fn print_scalar(ty: &Value) -> String {
    format!("{}scalar {}", print_description(ty, "", true), name_of(ty))
}

fn print_object(ty: &Value) -> String {
    format!(
        "{}type {}{}{}",
        print_description(ty, "", true),
        name_of(ty),
        print_implemented_interfaces(ty),
        print_fields(ty)
    )
}

fn print_interface(ty: &Value) -> String {
    format!(
        "{}interface {}{}{}",
        print_description(ty, "", true),
        name_of(ty),
        print_implemented_interfaces(ty),
        print_fields(ty)
    )
}

fn print_union(ty: &Value) -> String {
    let possible = ty
        .get("possibleTypes")
        .and_then(Value::as_array)
        .map(|types| sorted_by_name(types))
        .unwrap_or_default();
    let suffix = if possible.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = possible.iter().map(|t| name_of(t)).collect();
        format!(" = {}", names.join(" | "))
    };
    format!(
        "{}union {}{}",
        print_description(ty, "", true),
        name_of(ty),
        suffix
    )
}

fn print_enum(ty: &Value) -> String {
    let values = ty
        .get("enumValues")
        .and_then(Value::as_array)
        .map(|values| sorted_by_name(values))
        .unwrap_or_default();
    let items: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            format!(
                "{}{}{}{}",
                print_description(value, "  ", index == 0),
                "  ",
                name_of(value),
                print_deprecated(
                    value
                        .get("deprecationReason")
                        .and_then(Value::as_str)
                )
            )
        })
        .collect();
    format!(
        "{}enum {}{}",
        print_description(ty, "", true),
        name_of(ty),
        print_block(&items)
    )
}

fn print_input_object(ty: &Value) -> String {
    let fields = ty
        .get("inputFields")
        .and_then(Value::as_array)
        .map(|fields| sorted_by_name(fields))
        .unwrap_or_default();
    let items: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            format!(
                "{}{}{}",
                print_description(field, "  ", index == 0),
                "  ",
                print_input_value(field)
            )
        })
        .collect();
    format!(
        "{}input {}{}",
        print_description(ty, "", true),
        name_of(ty),
        print_block(&items)
    )
}

fn print_implemented_interfaces(ty: &Value) -> String {
    let interfaces = match ty.get("interfaces").and_then(Value::as_array) {
        Some(interfaces) if !interfaces.is_empty() => interfaces,
        _ => return String::new(),
    };
    let names: Vec<String> = sorted_by_name(interfaces).iter().map(|i| name_of(i)).collect();
    format!(" implements {}", names.join(" & "))
}

fn print_fields(ty: &Value) -> String {
    let fields = ty.get("fields").and_then(Value::as_array);
    let Some(fields) = fields else {
        return print_block(&[]);
    };
    let sorted = sorted_by_name(fields);
    let items: Vec<String> = sorted
        .iter()
        .enumerate()
        .map(|(index, field)| {
            format!(
                "{}{}{}{}: {}{}",
                print_description(field, "  ", index == 0),
                "  ",
                name_of(field),
                print_args(
                    field
                        .get("args")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    "  "
                ),
                type_ref_string(field.get("type").unwrap_or(&Value::Null)),
                print_deprecated(
                    field
                        .get("deprecationReason")
                        .and_then(Value::as_str)
                )
            )
        })
        .collect();
    print_block(&items)
}

fn print_args(args: &[Value], indentation: &str) -> String {
    if args.is_empty() {
        return String::new();
    }
    let sorted = sorted_by_name(args);

    // Every arg lacking a description: print them inline.
    if sorted.iter().all(|arg| !has_description(arg)) {
        let parts: Vec<String> = sorted.iter().map(|arg| print_input_value(arg)).collect();
        return format!("({})", parts.join(", "));
    }

    let parts: Vec<String> = sorted
        .iter()
        .enumerate()
        .map(|(index, arg)| {
            format!(
                "{}{}{}{}",
                print_description(arg, &format!("  {indentation}"), index == 0),
                "  ",
                indentation,
                print_input_value(arg)
            )
        })
        .collect();
    format!("(\n{}\n{})", parts.join("\n"), indentation)
}

fn has_description(value: &Value) -> bool {
    match value.get("description") {
        None => false,
        Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        _ => true,
    }
}

fn print_input_value(arg: &Value) -> String {
    let mut declaration = format!(
        "{}: {}",
        name_of(arg),
        type_ref_string(arg.get("type").unwrap_or(&Value::Null))
    );
    if let Some(default) = arg.get("defaultValue").and_then(Value::as_str) {
        declaration.push_str(" = ");
        declaration.push_str(default);
    }
    declaration.push_str(&print_deprecated(
        arg.get("deprecationReason").and_then(Value::as_str),
    ));
    declaration
}

fn print_directive(directive: &Value) -> String {
    let args = directive
        .get("args")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let locations: Vec<String> = directive
        .get("locations")
        .and_then(Value::as_array)
        .map(|locations| {
            let mut names: Vec<String> = locations
                .iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect();
            names.sort_by(|a, b| natural_compare(a, b));
            names
        })
        .unwrap_or_default();

    format!(
        "{}directive @{}{} on {}",
        print_description(directive, "", true),
        name_of(directive),
        print_args(args, ""),
        locations.join(" | ")
    )
}

fn print_deprecated(reason: Option<&str>) -> String {
    let Some(reason) = reason else {
        return String::new();
    };
    if reason != "No longer supported" {
        format!(" @deprecated(reason: {})", print_string(reason))
    } else {
        " @deprecated".to_string()
    }
}

fn type_ref_string(ty: &Value) -> String {
    match ty.get("kind").and_then(Value::as_str) {
        Some("NON_NULL") => {
            let inner = ty.get("ofType").unwrap_or(&Value::Null);
            format!("{}!", type_ref_string(inner))
        }
        Some("LIST") => {
            let inner = ty.get("ofType").unwrap_or(&Value::Null);
            format!("[{}]", type_ref_string(inner))
        }
        _ => ty
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn print_block(items: &[String]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!(" {{\n{}\n}}", items.join("\n"))
    }
}

fn print_description(def: &Value, indentation: &str, first_in_block: bool) -> String {
    let description = match def.get("description").and_then(Value::as_str) {
        Some(description) => description,
        None => return String::new(),
    };

    let block_string = if is_printable_as_block_string(description) {
        print_block_string(description, false)
    } else {
        print_string(description)
    };

    let prefix = if !indentation.is_empty() && !first_in_block {
        format!("\n{indentation}")
    } else {
        indentation.to_string()
    };
    let replaced = block_string.replace('\n', &format!("\n{indentation}"));
    format!("{prefix}{replaced}\n")
}

// --- GraphQL string printing (language/printString.js, blockString.js) ---

fn is_whitespace_char(c: char) -> bool {
    c == '\t' || c == ' '
}

fn is_printable_as_block_string(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    let mut is_empty_line = true;
    let mut has_indent = false;
    let mut has_common_indent = true;
    let mut seen_non_empty_line = false;

    for c in value.chars() {
        match c as u32 {
            0x0000..=0x0008 | 0x000b | 0x000c | 0x000e | 0x000f | 0x000d => return false,
            0x000a => {
                if is_empty_line && !seen_non_empty_line {
                    return false;
                }
                seen_non_empty_line = true;
                is_empty_line = true;
                has_indent = false;
            }
            0x0009 | 0x0020 => {
                if is_empty_line {
                    has_indent = true;
                }
            }
            _ => {
                if !has_indent {
                    has_common_indent = false;
                }
                is_empty_line = false;
            }
        }
    }

    if is_empty_line {
        return false;
    }
    if has_common_indent && seen_non_empty_line {
        return false;
    }
    true
}

fn print_block_string(value: &str, minimize: bool) -> String {
    let escaped = value.replace("\"\"\"", "\\\"\"\"");
    let lines: Vec<&str> = escaped.split('\n').collect();
    let is_single_line = lines.len() == 1;

    let force_leading_new_line = lines.len() > 1
        && lines[1..].iter().all(|line| {
            line.is_empty()
                || line
                    .chars()
                    .next()
                    .map(is_whitespace_char)
                    .unwrap_or(false)
        });

    let has_trailing_triple_quotes = escaped.ends_with("\\\"\"\"");
    let has_trailing_quote = value.ends_with('"') && !has_trailing_triple_quotes;
    let has_trailing_slash = value.ends_with('\\');
    let force_trailing_newline = has_trailing_quote || has_trailing_slash;

    let print_as_multiple_lines = !minimize
        && (!is_single_line
            || value.encode_utf16().count() > 70
            || force_trailing_newline
            || force_leading_new_line
            || has_trailing_triple_quotes);

    let mut result = String::new();
    let skip_leading_new_line = is_single_line
        && value
            .chars()
            .next()
            .map(is_whitespace_char)
            .unwrap_or(false);

    if (print_as_multiple_lines && !skip_leading_new_line) || force_leading_new_line {
        result.push('\n');
    }
    result.push_str(&escaped);
    if print_as_multiple_lines || force_trailing_newline {
        result.push('\n');
    }
    format!("\"\"\"{result}\"\"\"")
}

fn print_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for c in value.chars() {
        let code = c as u32;
        match code {
            0x08 => escaped.push_str("\\b"),
            0x09 => escaped.push_str("\\t"),
            0x0a => escaped.push_str("\\n"),
            0x0b => escaped.push_str("\\u000B"),
            0x0c => escaped.push_str("\\f"),
            0x0d => escaped.push_str("\\r"),
            0x22 => escaped.push_str("\\\""),
            0x5c => escaped.push_str("\\\\"),
            0x00..=0x1f | 0x7f..=0x9f => {
                escaped.push_str(&format!("\\u{code:04X}"));
            }
            _ => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

// --- Natural sort (jsutils/naturalCompare.js) ---

fn natural_compare(a: &str, b: &str) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut a_index = 0;
    let mut b_index = 0;

    while a_index < a_bytes.len() && b_index < b_bytes.len() {
        let a_char = a_bytes.get(a_index).copied();
        let b_char = b_bytes.get(b_index).copied();

        if is_digit(a_char) && is_digit(b_char) {
            let mut a_num: u64 = 0;
            let mut current = a_char;
            loop {
                a_index += 1;
                a_num = a_num * 10 + (current.unwrap() as u64 - 48);
                current = a_bytes.get(a_index).copied();
                if !(is_digit(current) && a_num > 0) {
                    break;
                }
            }

            let mut b_num: u64 = 0;
            let mut current = b_char;
            loop {
                b_index += 1;
                b_num = b_num * 10 + (current.unwrap() as u64 - 48);
                current = b_bytes.get(b_index).copied();
                if !(is_digit(current) && b_num > 0) {
                    break;
                }
            }

            if a_num < b_num {
                return Ordering::Less;
            }
            if a_num > b_num {
                return Ordering::Greater;
            }
        } else {
            let ac = a_char.unwrap_or(0) as u32;
            let bc = b_char.unwrap_or(0) as u32;
            if ac < bc {
                return Ordering::Less;
            }
            if ac > bc {
                return Ordering::Greater;
            }
            a_index += 1;
            b_index += 1;
        }
    }

    a.len().cmp(&b.len())
}

fn is_digit(code: Option<u8>) -> bool {
    matches!(code, Some(48..=57))
}
