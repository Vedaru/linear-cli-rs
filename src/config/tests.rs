use super::*;

#[test]
fn ignores_unrelated_and_broken_assignments() {
    let selected = select_relevant_assignments(
            "PATH=$PATH:/opt/bin\nexport LINEAR_API_KEY=abc\nJUNK=1\nGH_TOKEN=gh\nLINEAR_TEAM_ID=$TEAM\nLINEAR_QUOTED=\"a b\"\n",
        );
    assert_eq!(
        selected.text,
        "LINEAR_API_KEY=abc\nGH_TOKEN=gh\nLINEAR_QUOTED=\"a b\""
    );
    assert_eq!(selected.skipped_expansion_keys, vec!["LINEAR_TEAM_ID"]);
    assert!(selected.skipped_unterminated_keys.is_empty());
}

#[test]
fn self_referential_value_is_refused_not_hung() {
    // The exact line that hangs the upstream parser forever.
    let selected = select_relevant_assignments("LINEAR_PATH=$LINEAR_PATH:/opt/bin");
    assert!(selected.text.is_empty());
    assert_eq!(selected.skipped_expansion_keys, vec!["LINEAR_PATH"]);
}

#[test]
fn unterminated_quote_is_reported() {
    let selected = select_relevant_assignments("LINEAR_API_KEY=\"unterminated\n");
    assert!(selected.text.is_empty());
    assert_eq!(selected.skipped_unterminated_keys, vec!["LINEAR_API_KEY"]);
}

#[test]
fn shell_reference_detection() {
    assert!(has_shell_reference("${NAME}"));
    assert!(has_shell_reference("$NAME"));
    // A `#` opens a comment, so `$note` is never a reference. The real
    // caller passes `effective_value().text`, which strips the comment
    // before this check; mirror that here.
    let commented = effective_value("ENG # $note").unwrap();
    assert_eq!(commented.text, "ENG");
    assert!(!has_shell_reference(&commented.text));
    assert!(!has_shell_reference("a$"));
    assert!(!has_shell_reference("\\$NAME"));
}

#[test]
fn effective_value_forms() {
    assert_eq!(effective_value("\"a b\"").unwrap().text, "a b");
    assert!(!effective_value("'a $b'").unwrap().expands);
    assert_eq!(effective_value("bare # comment").unwrap().text, "bare");
    assert!(effective_value("\"open").is_none());
}

#[test]
fn dotenv_parsing_handles_quotes_and_escapes() {
    let vars = parse_dotenv("A=plain\nB='single $x'\nC=\"line\\nbreak\"\nD=trail # c");
    assert_eq!(vars.get("A").unwrap(), "plain");
    assert_eq!(vars.get("B").unwrap(), "single $x");
    assert_eq!(vars.get("C").unwrap(), "line\nbreak");
    assert_eq!(vars.get("D").unwrap(), "trail");
}

#[test]
fn bool_coercion_matches_strtobool() {
    assert_eq!(coerce_bool(&Value::Bool(true)), Some(true));
    assert_eq!(coerce_bool(&Value::String("YES".into())), Some(true));
    assert_eq!(coerce_bool(&Value::String("off".into())), Some(false));
    assert_eq!(coerce_bool(&Value::String("maybe".into())), None);
    assert_eq!(coerce_bool(&Value::Null), None);
}

#[test]
fn issue_sort_accepts_cli_and_rejects_invalid() {
    assert_eq!(
        resolve_issue_sort(Some("manual")).unwrap(),
        IssueSort::Manual
    );
    let error = resolve_issue_sort(Some("bogus")).unwrap_err();
    assert!(error.user_message.contains("Invalid issue sort"));
    assert!(error.suggestion.unwrap().contains("manual, priority"));
}
