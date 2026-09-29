//! `linear initiative comment add` — port of
//! `src/commands/initiative/initiative-comment-add.ts`.
//!
//! The comment subgroup `mod.rs` supplies the `Failed to add comment` context,
//! so this module returns bare errors.

use clap::Args;

use crate::comments::{
    self, CommentTarget, CreateCommentOptions, COMMENT_BODY_DESCRIPTION,
    COMMENT_BODY_FILE_DESCRIPTION, REPLY_TO_DESCRIPTION,
};
use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct InitiativeCommentAddArgs {
    /// Initiative ID, URL, slug ID, or name
    #[arg(value_name = "initiative")]
    pub initiative: String,
    #[arg(short = 'b', long, value_name = "text", help = COMMENT_BODY_DESCRIPTION)]
    pub body: Option<String>,
    #[arg(long, value_name = "path", help = COMMENT_BODY_FILE_DESCRIPTION)]
    pub body_file: Option<String>,
    #[arg(
        short = 'p',
        long,
        visible_alias = "reply-to",
        value_name = "commentId",
        help = REPLY_TO_DESCRIPTION
    )]
    pub parent: Option<String>,
}

pub fn run(args: InitiativeCommentAddArgs) -> Result<()> {
    // A comment attaches to the initiative's *id*, which the resolver returns
    // for a URL, UUID, slug ID, or exact name.
    let initiative = linear::resolve_initiative_id(&args.initiative)?;
    let text_body = comments::resolve_comment_body(args.body.as_deref(), args.body_file.as_deref())?;

    let comment_body = match text_body {
        Some(body) => body,
        None => comments::prompt_comment_body()?,
    };

    let (_id, url) = comments::create_comment(
        &CommentTarget::Initiative {
            initiative_id: initiative,
        },
        &CreateCommentOptions {
            body: comment_body,
            parent_id: args.parent.clone(),
            id: None,
        },
    )?;

    output::line(&format!("✓ Comment added to initiative {}", args.initiative));
    output::line(&url);
    Ok(())
}
