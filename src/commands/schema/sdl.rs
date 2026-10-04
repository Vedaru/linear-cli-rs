use std::cmp::Ordering;
use std::io::Write;

use super::{Directive, EnumValue, Field, InputValue, Schema, TypeDef, TypeRef, EMPTY_TYPE_REF};

// --- SDL printing (ports graphql-js buildClientSchema + lexicographicSortSchema
// --- + printSchema, operating on the borrowed introspection model) ---

/// Anything the SDL sorts by name. `TypeRef` has a nullable name, matching
/// graphql-js's `name ?? ''` treatment.
trait Named {
    fn name(&self) -> &str;
}

impl<'a> Named for TypeDef<'a> {
    fn name(&self) -> &str {
        self.name.as_ref()
    }
}

impl<'a> Named for Field<'a> {
    fn name(&self) -> &str {
        self.name.as_ref()
    }
}

impl<'a> Named for InputValue<'a> {
    fn name(&self) -> &str {
        self.name.as_ref()
    }
}

impl<'a> Named for EnumValue<'a> {
    fn name(&self) -> &str {
        self.name.as_ref()
    }
}

impl<'a> Named for Directive<'a> {
    fn name(&self) -> &str {
        self.name.as_ref()
    }
}

impl<'a> Named for TypeRef<'a> {
    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }
}

fn sorted_by_name<T: Named>(items: &[T]) -> Vec<&T> {
    let mut sorted: Vec<&T> = items.iter().collect();
    sorted.sort_by(|a, b| natural_compare(a.name(), b.name()));
    sorted
}

/// Render the SDL into `writer`, one top-level definition at a time.
///
/// This was `fn print_schema(&Schema) -> String`, which built every definition into a `Vec<String>`
/// and then `join`ed a second, equally large copy out of it - so the peak held the document about
/// twice over, and both call sites then handed the result to `write_all`. The per-type printers
/// still return a `String` each (they are small, and one is alive at a time), but nothing ever holds
/// the whole document: this writes each definition as it is produced. The comment at the file call
/// site used to *say* the output was written "straight into the file"; it was not, until now.
pub(super) fn print_schema_to<W: Write>(writer: &mut W, schema: &Schema<'_>) -> std::io::Result<()> {
    let query_name = schema.query_type.as_ref().and_then(|ty| ty.name.as_deref());
    let mutation_name = schema
        .mutation_type
        .as_ref()
        .and_then(|ty| ty.name.as_deref());
    let subscription_name = schema
        .subscription_type
        .as_ref()
        .and_then(|ty| ty.name.as_deref());

    // `schemaDescription` is not requested, so only a non-conventional root
    // naming forces an explicit `schema { ... }` block.
    let common_names = query_name.map_or(true, |n| n == "Query")
        && mutation_name.map_or(true, |n| n == "Mutation")
        && subscription_name.map_or(true, |n| n == "Subscription");

    // Definitions are separated by a blank line, and empty renderings are dropped - the same rule
    // the old `parts.retain(..).join("\n\n")` had, kept because the golden file depends on it.
    let mut first = true;

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
        write_part(
            writer,
            format!("schema {{\n{}\n}}", operation_types.join("\n")),
            &mut first,
        )?;
    }

    let mut custom: Vec<&Directive<'_>> = schema
        .directives
        .iter()
        .filter(|directive| !is_specified_directive(directive.name.as_ref()))
        .collect();
    custom.sort_by(|a, b| natural_compare(a.name.as_ref(), b.name.as_ref()));
    for directive in &custom {
        write_part(writer, print_directive(directive), &mut first)?;
    }

    let mut defined: Vec<&TypeDef<'_>> = schema
        .types
        .iter()
        .filter(|ty| is_defined_type(ty))
        .collect();
    defined.sort_by(|a, b| natural_compare(a.name.as_ref(), b.name.as_ref()));
    for ty in &defined {
        write_part(writer, print_type(ty), &mut first)?;
    }

    Ok(())
}

/// One definition, preceded by the blank line separator when it is not the first.
fn write_part<W: Write>(writer: &mut W, part: String, first: &mut bool) -> std::io::Result<()> {
    if part.is_empty() {
        return Ok(());
    }
    if *first {
        *first = false;
    } else {
        writer.write_all(b"\n\n")?;
    }
    writer.write_all(part.as_bytes())
}

fn is_specified_directive(name: &str) -> bool {
    matches!(
        name,
        "include" | "skip" | "deprecated" | "specifiedBy" | "oneOf"
    )
}

fn is_defined_type(ty: &TypeDef<'_>) -> bool {
    if ty.name.starts_with("__") {
        return false;
    }
    if ty.kind == "SCALAR"
        && matches!(
            ty.name.as_ref(),
            "Int" | "Float" | "String" | "Boolean" | "ID"
        )
    {
        return false;
    }
    true
}

fn print_type(ty: &TypeDef<'_>) -> String {
    // `kind` is a GraphQL identifier, but it is a `Cow` like every other string
    // so a response can never fail to parse over escaped-vs-borrowable text.
    match ty.kind.as_ref() {
        "SCALAR" => print_scalar(ty),
        "OBJECT" => print_object(ty),
        "INTERFACE" => print_interface(ty),
        "UNION" => print_union(ty),
        "ENUM" => print_enum(ty),
        "INPUT_OBJECT" => print_input_object(ty),
        _ => String::new(),
    }
}

fn print_scalar(ty: &TypeDef<'_>) -> String {
    format!(
        "{}scalar {}",
        print_description(ty.description.as_deref(), "", true),
        ty.name
    )
}

fn print_object(ty: &TypeDef<'_>) -> String {
    format!(
        "{}type {}{}{}",
        print_description(ty.description.as_deref(), "", true),
        ty.name,
        print_implemented_interfaces(ty),
        print_fields(ty)
    )
}

fn print_interface(ty: &TypeDef<'_>) -> String {
    format!(
        "{}interface {}{}{}",
        print_description(ty.description.as_deref(), "", true),
        ty.name,
        print_implemented_interfaces(ty),
        print_fields(ty)
    )
}

fn print_union(ty: &TypeDef<'_>) -> String {
    let possible = ty
        .possible_types
        .as_deref()
        .map(|types| sorted_by_name(types))
        .unwrap_or_default();
    let suffix = if possible.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = possible.iter().map(|t| t.name()).collect();
        format!(" = {}", names.join(" | "))
    };
    format!(
        "{}union {}{}",
        print_description(ty.description.as_deref(), "", true),
        ty.name,
        suffix
    )
}

fn print_enum(ty: &TypeDef<'_>) -> String {
    let values = ty
        .enum_values
        .as_deref()
        .map(|values| sorted_by_name(values))
        .unwrap_or_default();
    let items: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            format!(
                "{}{}{}{}",
                print_description(value.description.as_deref(), "  ", index == 0),
                "  ",
                value.name,
                print_deprecated(value.deprecation_reason.as_deref())
            )
        })
        .collect();
    format!(
        "{}enum {}{}",
        print_description(ty.description.as_deref(), "", true),
        ty.name,
        print_block(&items)
    )
}

fn print_input_object(ty: &TypeDef<'_>) -> String {
    let fields = ty
        .input_fields
        .as_deref()
        .map(|fields| sorted_by_name(fields))
        .unwrap_or_default();
    let items: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            format!(
                "{}{}{}",
                print_description(field.description.as_deref(), "  ", index == 0),
                "  ",
                print_input_value(field)
            )
        })
        .collect();
    format!(
        "{}input {}{}",
        print_description(ty.description.as_deref(), "", true),
        ty.name,
        print_block(&items)
    )
}

fn print_implemented_interfaces(ty: &TypeDef<'_>) -> String {
    let interfaces = match ty.interfaces.as_deref() {
        Some(interfaces) if !interfaces.is_empty() => interfaces,
        _ => return String::new(),
    };
    let names: Vec<&str> = sorted_by_name(interfaces)
        .iter()
        .map(|i| i.name())
        .collect();
    format!(" implements {}", names.join(" & "))
}

fn print_fields(ty: &TypeDef<'_>) -> String {
    let Some(fields) = ty.fields.as_deref() else {
        return print_block(&[]);
    };
    let sorted = sorted_by_name(fields);
    let items: Vec<String> = sorted
        .iter()
        .enumerate()
        .map(|(index, field)| {
            format!(
                "{}{}{}{}: {}{}",
                print_description(field.description.as_deref(), "  ", index == 0),
                "  ",
                field.name,
                print_args(&field.args, "  "),
                type_ref_string(&field.type_),
                print_deprecated(field.deprecation_reason.as_deref())
            )
        })
        .collect();
    print_block(&items)
}

fn print_args(args: &[InputValue<'_>], indentation: &str) -> String {
    if args.is_empty() {
        return String::new();
    }
    let sorted = sorted_by_name(args);

    // Every arg lacking a description: print them inline.
    if sorted
        .iter()
        .all(|arg| !has_description(arg.description.as_deref()))
    {
        let parts: Vec<String> = sorted.iter().map(|arg| print_input_value(arg)).collect();
        return format!("({})", parts.join(", "));
    }

    let parts: Vec<String> = sorted
        .iter()
        .enumerate()
        .map(|(index, arg)| {
            format!(
                "{}{}{}{}",
                print_description(
                    arg.description.as_deref(),
                    &format!("  {indentation}"),
                    index == 0,
                ),
                "  ",
                indentation,
                print_input_value(arg)
            )
        })
        .collect();
    format!("(\n{}\n{})", parts.join("\n"), indentation)
}

fn has_description(description: Option<&str>) -> bool {
    description.is_some_and(|text| !text.is_empty())
}

/// An argument or input field. The `InputValue` fragment does not select
/// `deprecationReason` (graphql-js's introspection query does not ask for input
/// deprecation), so unlike fields and enum values these never carry
/// `@deprecated` - matching what `buildClientSchema` can know here.
fn print_input_value(arg: &InputValue<'_>) -> String {
    let mut declaration = format!("{}: {}", arg.name, type_ref_string(&arg.type_));
    if let Some(default) = arg.default_value.as_deref() {
        declaration.push_str(" = ");
        declaration.push_str(default);
    }
    declaration
}

fn print_directive(directive: &Directive<'_>) -> String {
    let mut locations: Vec<&str> = directive.locations.iter().map(|l| l.as_ref()).collect();
    locations.sort_by(|a, b| natural_compare(a, b));

    format!(
        "{}directive @{}{} on {}",
        print_description(directive.description.as_deref(), "", true),
        directive.name,
        print_args(&directive.args, ""),
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

fn type_ref_string(ty: &TypeRef<'_>) -> String {
    match ty.kind.as_ref() {
        "NON_NULL" => format!("{}!", type_ref_string(inner_ref(ty))),
        "LIST" => format!("[{}]", type_ref_string(inner_ref(ty))),
        _ => ty.name.as_deref().unwrap_or("").to_string(),
    }
}

/// The wrapped type of a list/non-null ref.
fn inner_ref<'a, 'b>(ty: &'a TypeRef<'b>) -> &'a TypeRef<'b> {
    ty.of_type.as_deref().unwrap_or(&EMPTY_TYPE_REF)
}

fn print_block(items: &[String]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!(" {{\n{}\n}}", items.join("\n"))
    }
}

fn print_description(description: Option<&str>, indentation: &str, first_in_block: bool) -> String {
    let Some(description) = description else {
        return String::new();
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
            line.is_empty() || line.chars().next().map(is_whitespace_char).unwrap_or(false)
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
