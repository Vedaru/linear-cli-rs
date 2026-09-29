//! `linear template list` — port of `src/commands/template/template-list.ts`.

use std::io::IsTerminal;

use clap::Args;
use serde_json::Value;

use super::{
    fetch_templates, template_is_available_to, template_name, template_scope_label, template_type,
};
use crate::errors::Result;
use crate::{colors, display, linear, output};

#[derive(Args, Debug)]
pub struct TemplateListArgs {
    /// Only templates of this type (issue, project, or document)
    #[arg(
        long = "type",
        value_name = "type",
        value_parser = clap::builder::PossibleValuesParser::new(["issue", "project", "document"])
    )]
    pub template_type: Option<String>,
    /// Team key, name, or ID. Shows that team's templates plus workspace templates.
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: TemplateListArgs) -> Result<()> {
    let team = match &args.team {
        Some(reference) => Some(linear::resolve_team(reference)?),
        None => None,
    };

    let mut templates = fetch_templates()?;
    templates.retain(|template| {
        args.template_type
            .as_deref()
            .map(|wanted| template_type(template) == wanted)
            .unwrap_or(true)
    });
    if let Some(team) = &team {
        let team_ids = vec![team.id.clone()];
        templates.retain(|template| template_is_available_to(template, &team_ids));
    }

    // Stable display order: type, then name, workspace templates before team ones.
    templates.sort_by(|a, b| {
        template_type(a)
            .cmp(&template_type(b))
            .then_with(|| {
                template_name(a)
                    .to_lowercase()
                    .cmp(&template_name(b).to_lowercase())
            })
            .then_with(|| team_rank(a).cmp(&team_rank(b)))
            .then_with(|| team_key(a).cmp(&team_key(b)))
    });

    if args.json {
        output::print_json(&Value::Array(templates));
        return Ok(());
    }

    if templates.is_empty() {
        output::line("No templates found.");
        return Ok(());
    }

    let columns = terminal_columns();

    const ID_WIDTH: usize = 36;
    let type_width = templates
        .iter()
        .map(|template| display::display_width(&type_cell(template)))
        .max()
        .unwrap_or(0)
        .max(4);
    let team_width = templates
        .iter()
        .map(|template| display::display_width(&template_scope_label(template)))
        .max()
        .unwrap_or(0)
        .clamp(4, 15);
    const SPACE_WIDTH: usize = 3;
    let fixed = ID_WIDTH + type_width + team_width + SPACE_WIDTH;
    let max_name_width = templates
        .iter()
        .map(|template| display::display_width(&template_name(template)))
        .max()
        .unwrap_or(0)
        .max(4);
    let available_width = columns.saturating_sub(1 + fixed);
    let name_width = max_name_width.min(available_width.max(20));

    let header_cells = [
        display::pad_display("ID", ID_WIDTH),
        display::pad_display("NAME", name_width),
        display::pad_display("TYPE", type_width),
        display::pad_display("TEAM", team_width),
    ];
    let header = header_cells
        .iter()
        .map(|cell| colors::underline(cell))
        .collect::<Vec<_>>()
        .join(" ");
    output::line(&header);

    for template in &templates {
        let name = display::pad_display(
            &display::truncate_text(&template_name(template), name_width),
            name_width,
        );
        output::line(&format!(
            "{} {} {} {}",
            display::pad_display(&template_id_or_empty(template), ID_WIDTH),
            name,
            display::pad_display(&type_cell(template), type_width),
            display::pad_display(&template_scope_label(template), team_width),
        ));
    }

    output::blank();
    output::line(&format!(
        "{} {} found.",
        templates.len(),
        if templates.len() == 1 {
            "template"
        } else {
            "templates"
        }
    ));
    Ok(())
}

fn template_id_or_empty(template: &Value) -> String {
    template
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn type_cell(template: &Value) -> String {
    let template_type = template_type(template);
    if template
        .get("hasFormFields")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        format!("{template_type} (form)")
    } else {
        template_type
    }
}

fn team_rank(template: &Value) -> u8 {
    if template.get("team").filter(|team| !team.is_null()).is_some() {
        1
    } else {
        0
    }
}

fn team_key(template: &Value) -> String {
    template
        .get("team")
        .filter(|team| !team.is_null())
        .and_then(|team| team.get("key"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn terminal_columns() -> usize {
    if std::io::stdout().is_terminal() {
        if let Some((width, _)) = terminal_size::terminal_size() {
            return width.0 as usize;
        }
    }
    120
}
