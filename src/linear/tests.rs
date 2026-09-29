use super::prelude::*;
use super::*;

    fn state(id: &str, name: &str, state_type: &str, position: f64) -> WorkflowState {
        WorkflowState {
            id: id.into(),
            name: name.into(),
            state_type: state_type.into(),
            position,
        }
    }

    #[test]
    fn sorts_type_groups_then_position_descending() {
        let mut states = [
            state("1", "Backlog", "backlog", 1.0),
            state("2", "In Progress", "started", 1.0),
            state("3", "In Review", "started", 2.0),
            state("4", "Todo", "unstarted", 1.0),
        ];
        states.sort_by(compare_workflow_states);
        let names: Vec<&str> = states.iter().map(|s| s.name.as_str()).collect();
        // started leads, position descending within it; then unstarted; then
        // backlog.
        assert_eq!(names, ["In Review", "In Progress", "Todo", "Backlog"]);
    }

    #[test]
    fn lowest_position_is_independent_of_input_order() {
        let states = vec![
            state("3", "In Review", "started", 2.0),
            state("2", "In Progress", "started", 1.0),
        ];
        assert_eq!(
            lowest_position_state_of_type(&states, "started").unwrap().name,
            "In Progress"
        );
    }

    #[test]
    fn resolve_prefers_exact_name_over_type() {
        // A state literally named "Started" shadows the bare `started` type
        // token: the name lookup runs first, exactly as upstream does.
        let states = vec![
            state("1", "Started", "started", 5.0),
            state("2", "In Progress", "started", 1.0),
        ];
        assert_eq!(
            resolve_workflow_state(&states, "Started").unwrap().unwrap().id,
            "1"
        );
        assert_eq!(
            resolve_workflow_state(&states, "started").unwrap().unwrap().id,
            "1"
        );
    }

    #[test]
    fn resolve_by_bare_type_uses_lowest_position() {
        // Neither name equals the type token, so this is a type lookup: it
        // resolves to the lowest-position state of that type, independent of
        // the order of `states`.
        let states = vec![
            state("3", "In Review", "started", 2.0),
            state("2", "In Progress", "started", 1.0),
        ];
        assert_eq!(
            resolve_workflow_state(&states, "started").unwrap().unwrap().id,
            "2"
        );
    }

    #[test]
    fn not_found_error_lists_valid_states() {
        let states = vec![state("1", "Todo", "unstarted", 1.0)];
        let error = workflow_state_not_found_error("ENG", "nope", &states);
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert!(error.user_message.contains("nope"));
        assert!(error.suggestion.unwrap().contains("\"Todo\" (unstarted)"));
    }

    #[test]
    fn bare_integer_detection() {
        assert!(is_bare_integer("123"));
        assert!(!is_bare_integer("0"));
        assert!(!is_bare_integer("07"));
        assert!(!is_bare_integer("1a"));
    }

    #[test]
    fn signed_integer_detection() {
        assert!(is_signed_integer("+1"));
        assert!(is_signed_integer("-12"));
        assert!(!is_signed_integer("1"));
        assert!(!is_signed_integer("+"));
        assert!(!is_signed_integer("-1a"));
    }

    #[test]
    fn uuid_detection_is_case_insensitive() {
        assert!(is_linear_uuid("123e4567-e89b-12d3-a456-426614174000"));
        assert!(is_linear_uuid("123E4567-E89B-12D3-A456-426614174000"));
        assert!(!is_linear_uuid("ENG-123"));
        assert!(!is_linear_uuid("123e4567e89b12d3a456426614174000"));
    }

    #[test]
    fn blocked_only_when_a_blocker_is_open() {
        let blocked = json!({
            "inverseRelations": {
                "nodes": [
                    { "type": "duplicate", "issue": { "state": { "type": "started" } } },
                    { "type": "blocks", "issue": { "state": { "type": "started" } } }
                ]
            }
        });
        assert!(is_issue_blocked(&blocked));
    }

    #[test]
    fn completed_blocker_does_not_count() {
        let unblocked = json!({
            "inverseRelations": {
                "nodes": [
                    { "type": "blocks", "issue": { "state": { "type": "completed" } } },
                    { "type": "blocks", "issue": { "state": { "type": "canceled" } } }
                ]
            }
        });
        assert!(!is_issue_blocked(&unblocked));
    }

    #[test]
    fn outgoing_relation_does_not_count_as_blocked() {
        // The old bug: a "blocks" relation on the issue itself means the issue
        // blocks another, not that it is blocked.
        let blocks_other = json!({
            "relations": { "nodes": [{ "type": "blocked_by" }] }
        });
        assert!(!is_issue_blocked(&blocks_other));
        assert!(!is_issue_blocked(&json!({})));
    }

    #[test]
    fn date_filters_normalise_to_iso() {
        assert_eq!(
            parse_date_filter("2024-01-15", "--created-after").unwrap(),
            "2024-01-15T00:00:00.000Z"
        );
        assert_eq!(
            parse_date_filter("2024-01-15T09:30:00Z", "--updated-after").unwrap(),
            "2024-01-15T09:30:00.000Z"
        );
        assert_eq!(
            parse_date_filter("2024-01-15T09:30:00+05:00", "--updated-after").unwrap(),
            "2024-01-15T04:30:00.000Z"
        );
    }

    #[test]
    fn date_filter_rejects_bad_input() {
        let error = parse_date_filter("15-01-2024", "--created-after").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.user_message.contains("--created-after"));
    }

    #[test]
    fn workflow_state_filter_builds_or_clauses() {
        let selection = StateSelection {
            types: vec!["started".to_string()],
            state_ids: vec![],
        };
        let filter = workflow_state_filter(&selection).unwrap().unwrap();
        assert_eq!(filter["state"]["type"]["in"][0], "started");
        assert!(workflow_state_filter(&StateSelection::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn label_filter_single_name_uses_some_match() {
        assert_eq!(
            label_filter(&["bug".to_string()]),
            Some(json!({ "some": { "name": { "eqIgnoreCase": "bug" } } }))
        );
        assert_eq!(label_filter(&[]), None);
    }

    #[test]
    fn label_filter_multiple_names_are_anded() {
        assert_eq!(
            label_filter(&["bug".to_string(), "p1".to_string()]),
            Some(json!({
                "and": [
                    { "some": { "name": { "eqIgnoreCase": "bug" } } },
                    { "some": { "name": { "eqIgnoreCase": "p1" } } },
                ]
            }))
        );
    }

    #[test]
    fn issue_sort_payload_matches_mode() {
        assert_eq!(
            get_issue_sort_payload(IssueSort::Manual),
            json!([{ "workflowState": { "order": "Ascending" } }])
        );
        assert_eq!(
            get_issue_sort_payload(IssueSort::Priority),
            json!([{ "priority": { "order": "Ascending" } }])
        );
    }

    #[test]
    fn scoped_state_error_lists_available_states() {
        let scope = StateScope::TeamKeys(vec!["ENG".to_string()]);
        let states = vec![
            ScopedWorkflowState {
                id: "1".into(),
                name: "Todo".into(),
                state_type: "unstarted".into(),
                team_key: "ENG".into(),
            },
            ScopedWorkflowState {
                id: "2".into(),
                name: "In Progress".into(),
                state_type: "started".into(),
                team_key: "ENG".into(),
            },
        ];
        let error = state_not_found_in_scope_error("nope", &scope, &states);
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert!(error.user_message.contains("\"ENG\""));
        assert!(error.suggestion.unwrap().contains("In Progress (ENG)"));
    }
