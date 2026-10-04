//! `linear schema` — print the GraphQL schema.
//!
//! Port of `src/commands/schema.ts`. Upstream fetches `getIntrospectionQuery()`
//! and then runs `buildClientSchema` → `lexicographicSortSchema` → `printSchema`
//! from graphql-js to emit SDL. This port fetches the same introspection
//! document and reproduces those three transformations, so the SDL matches
//! graphql-js byte for byte: types/fields/args/enum values are natural-sorted,
//! specified scalars and `__*` introspection types are omitted, and
//! descriptions are printed as GraphQL block strings.
//!
//! The document is parsed into **borrowed** structs - every string is a
//! `Cow<'a, str>` that borrows out of the response body (which stays alive for
//! the whole command) and only allocates for the escapable strings serde_json
//! cannot hand out as a slice - instead of a `serde_json::Value` tree. Linear's introspection
//! response is ~2.7 MB of JSON and a generic tree for it costs ~25 MB of heap,
//! which dominated this command's peak. The structs below cover exactly the
//! fields [`INTROSPECTION_QUERY`] selects and declare them in the order the API
//! returns them, so `--json` is re-serialised from the same model and comes out
//! byte for byte identical.
//!
//! `--json` bypasses SDL and pretty-prints the introspection result; `-o/--output`
//! writes either form to a file, appending exactly one newline.

use std::borrow::Cow;
use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::errors::{CliError, Result};
use crate::graphql;
use crate::output;

mod sdl;

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

// --- Introspection response model ---
//
// Exactly the fields `INTROSPECTION_QUERY` selects, in the order the API returns
// them (which is the query's selection order), because `--json` re-serialises
// this model with `to_writer_pretty` and serde emits struct fields in
// declaration order. Nothing is skipped when serialising, so a `null` in the
// response stays a `null` in the output.
//
// `Option` mirrors the introspection schema's nullability rather than a missing
// key: the API always returns every selected field, with `null` where there is
// nothing to report.

/// The whole response document: only `data` is needed, but `errors` is not
/// selected here because [`graphql::Client::request_raw`] has already rejected
/// any response carrying one.
#[derive(Deserialize)]
struct IntrospectionResponse<'a> {
    #[serde(borrow)]
    data: ResponseData<'a>,
}

#[derive(Deserialize)]
struct ResponseData<'a> {
    #[serde(borrow, rename = "__schema")]
    schema: Option<Schema<'a>>,
}

#[derive(Deserialize, Serialize)]
struct Schema<'a> {
    #[serde(borrow, rename = "queryType")]
    query_type: Option<RootType<'a>>,
    #[serde(borrow, rename = "mutationType")]
    mutation_type: Option<RootType<'a>>,
    #[serde(borrow, rename = "subscriptionType")]
    subscription_type: Option<RootType<'a>>,
    #[serde(borrow)]
    types: Vec<TypeDef<'a>>,
    #[serde(borrow)]
    directives: Vec<Directive<'a>>,
}

/// `{ name kind }` for a root operation type.
#[derive(Deserialize, Serialize)]
struct RootType<'a> {
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    kind: Cow<'a, str>,
}

#[derive(Deserialize, Serialize)]
struct TypeDef<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    description: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    fields: Option<Vec<Field<'a>>>,
    #[serde(borrow, default, rename = "inputFields")]
    input_fields: Option<Vec<InputValue<'a>>>,
    #[serde(borrow, default)]
    interfaces: Option<Vec<TypeRef<'a>>>,
    #[serde(borrow, default, rename = "enumValues")]
    enum_values: Option<Vec<EnumValue<'a>>>,
    #[serde(borrow, default, rename = "possibleTypes")]
    possible_types: Option<Vec<TypeRef<'a>>>,
}

#[derive(Deserialize, Serialize)]
struct Field<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    description: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    args: Vec<InputValue<'a>>,
    #[serde(borrow, rename = "type")]
    type_: TypeRef<'a>,
    #[serde(rename = "isDeprecated")]
    is_deprecated: bool,
    #[serde(borrow, rename = "deprecationReason")]
    deprecation_reason: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Serialize)]
struct InputValue<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    description: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "type")]
    type_: TypeRef<'a>,
    #[serde(borrow, default, rename = "defaultValue")]
    default_value: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Serialize)]
struct EnumValue<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    description: Option<Cow<'a, str>>,
    #[serde(rename = "isDeprecated")]
    is_deprecated: bool,
    #[serde(borrow, rename = "deprecationReason")]
    deprecation_reason: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Serialize)]
struct Directive<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    description: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    locations: Vec<Cow<'a, str>>,
    #[serde(borrow, default)]
    args: Vec<InputValue<'a>>,
}

#[derive(Deserialize, Serialize)]
struct TypeRef<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow, default, rename = "ofType")]
    of_type: Option<Box<TypeRef<'a>>>,
}

/// `--json` prints the whole `data` object, whose only member is `__schema`.
#[derive(Serialize)]
struct SchemaJson<'a> {
    #[serde(rename = "__schema")]
    schema: &'a Schema<'a>,
}

/// Stand-in used when a wrapper type's `ofType` is missing. graphql-js never
/// emits that, but the pre-refactor printer rendered such a ref as an empty
/// name, and this keeps that behaviour rather than unwrapping.
static EMPTY_TYPE_REF: TypeRef<'static> = TypeRef {
    kind: Cow::Borrowed(""),
    name: None,
    of_type: None,
};

pub fn run(args: SchemaArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to fetch schema"))
}

fn run_inner(args: SchemaArgs) -> Result<()> {
    let client = graphql::client()?;
    // `body` is kept alive for the whole command: every string in the model
    // below borrows out of it, so nothing is copied per type/field/arg.
    let body = client.request_raw(INTROSPECTION_QUERY, json!({}))?;
    let response: IntrospectionResponse<'_> = serde_json::from_slice(&body)?;

    let schema =
        response.data.schema.as_ref().ok_or_else(|| {
            CliError::cli("Introspection response did not contain a __schema field")
        })?;

    match &args.output {
        Some(path) => {
            // Written straight into the file: the introspection JSON is several MB
            // and the SDL about 1 MB, so rendering the whole document into a
            // String first is a copy of the output that nothing reads.
            //
            // Into a *staging* file, though: the command's own cache-shaped output must not be
            // observable half-written either, and a reader that arrives mid-render would otherwise
            // read a truncated schema and believe it.
            let staged = crate::atomic::staging_path(std::path::Path::new(path));
            let mut file = std::fs::File::create(&staged)
                .map_err(|error| CliError::cli(format!("Failed to write {path}")).cause(error))?;
            let written = if args.json {
                write_json_schema(&mut file, schema)
            } else {
                sdl::print_schema_to(&mut file, schema)
            };
            written
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|error| CliError::cli(format!("Failed to write {path}")).cause(error))?;
            crate::atomic::commit(&staged, std::path::Path::new(path))?;
            output::line(&format!("Schema written to {path}"));
        }
        None => {
            if args.json {
                // Streamed rather than built as a String first.
                print_json_schema(schema);
            } else {
                // Streamed too, definition by definition - see `print_schema_to`.
                print_sdl_schema(schema);
            }
        }
    }
    Ok(())
}

/// Print the SDL to stdout, definition by definition.
///
/// The stdout sibling of `print_schema_to`: same reason it exists, same "write errors are ignored
/// on the stdout path" rule as `print_json_schema` below, and the trailing newline the old
/// `output::line(...)` used to add.
fn print_sdl_schema(schema: &Schema<'_>) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if sdl::print_schema_to(&mut lock, schema).is_err() {
        return;
    }
    let _ = writeln!(lock);
}

/// Serialise the introspection result the way `output::print_json` does, but
/// from the borrowed model instead of a `Value` tree. Write errors are ignored,
/// as they are everywhere else on the stdout path.
fn print_json_schema(schema: &Schema<'_>) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if write_json_schema(&mut lock, schema).is_err() {
        return;
    }
    let _ = writeln!(lock);
}

/// Write the introspection result with two-space indentation to any writer.
fn write_json_schema<W: Write>(writer: &mut W, schema: &Schema<'_>) -> std::io::Result<()> {
    serde_json::to_writer_pretty(writer, &SchemaJson { schema }).map_err(std::io::Error::other)
}

