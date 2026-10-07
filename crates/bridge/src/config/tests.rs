use super::*;

#[test]
fn a_tilde_is_expanded_from_whichever_home_this_platform_names() {
    assert_eq!(
        expand_tilde_with("~/x/bridge.db", Some("/home/dev")),
        "/home/dev/x/bridge.db"
    );
    // Windows names the same thing USERPROFILE, and a joined path is absolute there
    // whatever separator the join used.
    assert_eq!(
        expand_tilde_with("~/x/bridge.db", Some(r"C:\Users\dev")),
        r"C:\Users\dev/x/bridge.db"
    );
    // Nothing to expand against: the operator's path, left exactly as it is.
    assert_eq!(expand_tilde_with("~/x/bridge.db", None), "~/x/bridge.db");
    assert_eq!(
        expand_tilde_with("/already/absolute.db", Some("/home/dev")),
        "/already/absolute.db"
    );
}

fn document(extra: &str) -> String {
    format!(
        r#"
api_key = "lin_api_ignored_by_the_service"
team_id = "VED"

[bridge]
bind = "127.0.0.1:9999"
store = "/tmp/bridge.db"

[platform.forgejo]
type = "forgejo"
secret_env = "BRIDGE_TEST_SECRET"
token_env = "BRIDGE_TEST_TOKEN"
closed_state = ["closed"]
open_state = "open"

[platform.linear]
type = "linear"
secret = "0123456789abcdef"
token_env = "BRIDGE_TEST_TOKEN"
closed_state = ["Done", "Canceled"]
open_state = "In Progress"
initial_state = "Todo"

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
{extra}
"#
    )
}

/// The document without the mapping, for the cases that publish their own.
#[test]
fn an_identity_map_reaches_the_reconciler() {
    let config = parse(&document(
        "\n[[mapping.identity]]\nlinear = \"loner@example.com\"\nforgejo = \"vedaru\"\n",
    ))
    .expect("the config loads");

    let mappings = config.reconcile_mappings().expect("the mapping resolves");
    let users = &mappings[0].users;
    assert_eq!(users.len(), 1);
    let on_forge = users
        .counterpart_for(
            &Identity::new("linear", "loner@example.com"),
            &ConnectorId::new("forgejo"),
        )
        .expect("known on the forge");
    assert_eq!(on_forge.key, "vedaru");
    assert_eq!(users.describe().len(), 1);
}

#[test]
fn an_identity_for_one_platform_only_is_refused() {
    // It would match nothing and skip every assignee in silence, so it is a load
    // error rather than a half-known person.
    // The document loads; it is resolving the mapping that refuses, which is
    // where every other mapping-shaped error is caught.
    let config = parse(&document(
        "\n[[mapping.identity]]\nlinear = \"loner@example.com\"\n",
    ))
    .expect("the document itself is valid");
    let error = config
        .reconcile_mappings()
        .expect_err("a one-platform identity is not an identity");
    let message = error.to_string();
    assert!(message.contains("at least two"), "{message}");
}

#[test]
fn an_identity_naming_platforms_this_mapping_does_not_connect_is_refused() {
    let config = parse(&document(
        "\n[[mapping.identity]]\ncodeberg = \"vedaru\"\ngitea = \"vedaru\"\n",
    ))
    .expect("the document itself is valid");
    let error = config
        .reconcile_mappings()
        .expect_err("no identity names either end");
    let message = error.to_string();
    assert!(message.contains("no identity names"), "{message}");
    assert!(message.contains("linear-cli-rs"), "{message}");
}

fn document_without_mapping() -> String {
    let text = document("");
    let start = text.find("[[mapping]]").expect("the fixture has a mapping");
    text[..start].to_string()
}

/// Parses with a deterministic environment.
///
/// Deliberately *not* the process environment: tests run in parallel inside
/// one process, so a case that sets a variable for its own benefit is a case
/// that breaks every other test running at the same moment.
fn parse(text: &str) -> Result<BridgeConfig> {
    BridgeConfig::from_toml_with_env(text, &env)
}

fn env(name: &str) -> Option<String> {
    match name {
        "BRIDGE_TEST_SECRET" => Some("0123456789abcdef".to_string()),
        "BRIDGE_TEST_TOKEN" => Some("gto_0123456789abcdef".to_string()),
        _ => None,
    }
}

#[test]
fn parses_the_service_sections_and_ignores_the_cli_ones() {
    let config = parse(&document("")).expect("valid document");
    assert_eq!(config.bind.to_string(), "127.0.0.1:9999");
    assert_eq!(config.store_path, "/tmp/bridge.db");
    assert_eq!(config.platforms.len(), 2);
    assert_eq!(config.mappings.len(), 1);
    assert_eq!(config.mappings[0].source, "linear:VED");
    assert!(config.mappings[0].sync_issues);
    assert!(!config.mappings[0].delete_sync);
    assert_eq!(config.sources().len(), 2);
}

#[test]
fn a_preset_type_loads_the_preset_spec() {
    let config = parse(&document("")).unwrap();
    let forgejo = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "forgejo")
        .unwrap();
    assert_eq!(forgejo.declared_type, "forgejo");
    // The forgejo preset's signature headers are the ones the forge sends.
    assert_eq!(
        forgejo.spec.signature.headers,
        vec!["x-forgejo-signature", "x-gitea-signature"]
    );
}

#[test]
fn the_state_vocabulary_and_the_direction_come_from_the_config() {
    let config = parse(&document("direction = \"oneway\"")).unwrap();

    let linear = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "linear")
        .unwrap();
    assert_eq!(linear.states.closed, vec!["Done", "Canceled"]);
    assert_eq!(linear.states.initial.as_deref(), Some("Todo"));
    assert_eq!(linear.states.open.as_deref(), Some("In Progress"));

    let forgejo = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "forgejo")
        .unwrap();
    assert_eq!(forgejo.states.closed, vec!["closed"]);
    assert_eq!(forgejo.states.initial, None);

    assert_eq!(config.mappings[0].direction, Direction::SourceToSink);
    assert_eq!(
        config.mappings[0].policy(linear, forgejo).direction,
        Direction::SourceToSink
    );
}

#[test]
fn a_direction_nobody_implements_is_refused_by_name() {
    let error = parse(&document("direction = \"sideways\""))
        .unwrap_err()
        .to_string();
    assert!(error.contains("sideways"), "{error}");
    assert!(error.contains("oneway"), "{error}");
}

#[test]
fn both_ends_of_a_mapping_need_a_credential() {
    // The forgejo platform loses its token, and only its own line is touched:
    // the mapping cannot read it, so it cannot be carried out.
    let text = document("").replace(
        "secret_env = \"BRIDGE_TEST_SECRET\"\ntoken_env = \"BRIDGE_TEST_TOKEN\"",
        "secret_env = \"BRIDGE_TEST_SECRET\"",
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("forgejo"), "{error}");
    assert!(error.contains("token_env"), "{error}");
    assert!(
        error.contains("webhook secret"),
        "the message distinguishes the two credentials: {error}"
    );
}

#[test]
fn a_platform_without_a_token_is_a_source_only() {
    // Intake needs no credential, so a read-only platform is a valid deployment;
    // it simply cannot take part in a mapping.
    let config = parse(&document_without_mapping()).unwrap();
    assert_eq!(config.sources().len(), 2, "both still accept webhooks");
    assert_eq!(config.sinks().len(), 2, "and both have a token here");

    let text = document_without_mapping().replace(
            "[platform.linear]\ntype = \"linear\"\nsecret = \"0123456789abcdef\"\ntoken_env = \"BRIDGE_TEST_TOKEN\"",
            "[platform.linear]\ntype = \"linear\"\nsecret = \"0123456789abcdef\"",
        );
    let config = parse(&text).unwrap();
    assert_eq!(config.sinks().len(), 1, "only the forge has a credential");
}

#[test]
fn an_unset_token_variable_is_a_startup_error_naming_it() {
    let error = BridgeConfig::from_toml_with_env(&document(""), &|name| {
        (name == "BRIDGE_TEST_SECRET").then(|| "0123456789abcdef".to_string())
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("BRIDGE_TEST_TOKEN"), "{error}");
    assert!(error.contains("not set"), "{error}");
}

#[test]
fn a_platform_may_point_at_another_api_address() {
    let text = document("").replace(
        "[platform.forgejo]",
        "[platform.forgejo]\napi_url = \"http://127.0.0.1:4000/api/v1\"",
    );
    let config = parse(&text).unwrap();
    let forgejo = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "forgejo")
        .unwrap();
    // The preset's own default is the real address; the deployment's override
    // replaces it, and the rest of the write half is untouched.
    assert_eq!(
        forgejo.sink_spec().unwrap().base_url,
        "http://127.0.0.1:4000/api/v1"
    );
    assert!(forgejo.sink_spec().unwrap().issue.create.is_some());
    assert!(forgejo.can_be_written());
}

#[test]
fn the_mappings_resolve_into_the_reconcilers_terms() {
    let config = parse(&document("")).unwrap();
    let mappings = config.reconcile_mappings().unwrap();

    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].source.describe(), "linear:VED");
    assert_eq!(mappings[0].sink.describe(), "forgejo:Vedaru/linear-cli-rs");
    // The policy carries the two vocabularies, which is what lets the reconciler
    // compare `Done` with `closed` without either platform knowing the other.
    assert_eq!(
        mappings[0].policy.names.source.closed,
        vec!["Done", "Canceled"]
    );
    assert_eq!(mappings[0].policy.names.sink.closed, vec!["closed"]);
    assert_eq!(mappings[0].policy.names.sink.initial, None);
}

#[test]
fn a_generic_platform_can_describe_itself_in_the_config() {
    let text = document(
        r#"
[platform.internal]
type = "generic"
secret_env = "BRIDGE_TEST_SECRET"
[platform.internal.spec.signature]
headers = ["x-signature"]
algorithm = "token"
[platform.internal.spec.event]
headers = ["x-event"]
[[platform.internal.spec.event.rule]]
match = "ticket"
kind = "issue"
[platform.internal.spec.event.rule.fields]
id = "/ticket/id"
"#,
    );
    let config = parse(&text).unwrap();
    let internal = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "internal")
        .unwrap();
    assert_eq!(
        internal.spec.signature.algorithm,
        crate::connector::Algorithm::Token
    );
    assert_eq!(internal.spec.event.rules.len(), 1);
    // And it is a usable source, built by the same engine as the presets.
    assert_eq!(config.sources().len(), 3);
}

#[test]
fn defaults_are_applied_when_the_bridge_section_is_minimal() {
    let text = document("").replace("bind = \"127.0.0.1:9999\"\n", "");
    let config = parse(&text).unwrap();
    assert_eq!(config.bind.to_string(), DEFAULT_BIND);
    assert_eq!(config.body_limit, DEFAULT_BODY_LIMIT);
    assert_eq!(
        config.worker.max_attempts,
        WorkerConfig::default().max_attempts
    );
}

#[test]
fn an_unknown_platform_type_names_the_alternatives() {
    let text = document("").replace("type = \"forgejo\"", "type = \"bitbucket\"");
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("unknown platform type"), "{error}");
    assert!(error.contains("linear"), "{error}");
}

#[test]
fn a_missing_secret_is_a_startup_error_not_a_runtime_one() {
    // An empty environment: the point of the case is that a referenced but
    // unset variable fails at startup, naming the variable.
    let error = BridgeConfig::from_toml_with_env(&document(""), &|_| None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("BRIDGE_TEST_SECRET"), "{error}");
    assert!(error.contains("not set"), "{error}");
}

#[test]
fn a_short_secret_is_refused() {
    let text = document("").replace("secret = \"0123456789abcdef\"", "secret = \"short\"");
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("at least 16"), "{error}");
}

/// Why a config was refused as a webhook receiver, or a panic saying it was not.
fn source_refusal(config: &BridgeConfig) -> String {
    match config.receiving_sources() {
        Ok(sources) => panic!("a config with no secret built {} sources", sources.len()),
        Err(error) => error.to_string(),
    }
}

/// Strip the secret lines from the fixture, leaving its credentials.
fn cli_only(document: &str) -> String {
    document
        .lines()
        .filter(|line| !line.starts_with("secret"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_config_with_no_secrets_is_valid_for_a_sweep() {
    // A deployment that only pushes changes has no use for a webhook secret: no
    // service, nothing to verify a delivery against, nothing to configure. The
    // file has to load, and its mappings have to resolve, with none.
    let config = parse(&cli_only(&document(""))).expect("a CLI-only config loads");

    assert_eq!(
        config.sinks().len(),
        2,
        "credentials are everything the write path needs"
    );
    assert_eq!(
        config
            .reconcile_mappings()
            .expect("the mapping resolves")
            .len(),
        1
    );
}

#[test]
fn asking_a_cli_only_config_to_receive_names_the_platform() {
    // The requirement lives where it belongs - building a webhook source, which is
    // what `webhook serve` does and `linear sync` does not.
    let both = parse(&cli_only(&document(""))).expect("the config loads");
    let error = source_refusal(&both);
    assert!(error.contains("[platform.forgejo]"), "{error}");
    assert!(error.contains("no webhook secret"), "{error}");
    assert!(error.contains("secret_env"), "the remedy: {error}");
    assert!(
        error.contains("linear sync"),
        "and the way that needs no secret: {error}"
    );

    // Per platform, not per file: the one that is missing a secret is the one named.
    let one = parse(&document("").replace("secret = \"0123456789abcdef\"\n", ""))
        .expect("the config loads");
    let error = source_refusal(&one);
    assert!(error.contains("[platform.linear]"), "{error}");
    assert!(!error.contains("[platform.forgejo]"), "{error}");
}

#[test]
fn a_mapping_naming_an_undeclared_platform_is_refused() {
    let text = document("").replace(
        "sink = \"forgejo:Vedaru/linear-cli-rs\"",
        "sink = \"codeberg:o/r\"",
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("not declared"), "{error}");
}

#[test]
fn a_mapping_without_a_scope_is_refused() {
    let text = document("").replace("source = \"linear:VED\"", "source = \"linear\"");
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("connector:scope"), "{error}");
}

#[test]
fn a_config_with_no_platform_is_refused() {
    let text = r#"
[bridge]
bind = "127.0.0.1:1"
"#;
    let error = parse(text).unwrap_err().to_string();
    assert!(error.contains("nothing to accept"), "{error}");
}

#[test]
fn an_invalid_bind_names_the_value() {
    let text = document("").replace("127.0.0.1:9999", "not-an-address");
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("not-an-address"), "{error}");
}

#[test]
fn an_unknown_key_inside_a_section_is_refused_rather_than_ignored() {
    let text = document("").replace(
        "[platform.forgejo]",
        "[platform.forgejo]\nunexpected = true",
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("unexpected"), "{error}");
}

#[test]
fn a_generic_platform_without_a_spec_is_refused() {
    let text = document(
        r#"
[platform.thing]
type = "generic"
secret_env = "BRIDGE_TEST_SECRET"
"#,
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("no spec"), "{error}");
}

#[test]
fn a_preset_with_an_inline_spec_is_refused_as_ambiguous() {
    let text = document(
        r#"
[platform.linear.spec.signature]
headers = ["x"]
"#,
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("also declares a spec"), "{error}");
}

#[test]
fn a_broken_inline_spec_fails_at_startup_with_its_reason() {
    let text = document(
        r#"
[platform.internal]
type = "generic"
secret_env = "BRIDGE_TEST_SECRET"
[platform.internal.spec.signature]
headers = ["x"]
[platform.internal.spec.event]
headers = ["x-event"]
[[platform.internal.spec.event.rule]]
match = "ticket"
kind = "ticketing"
[platform.internal.spec.event.rule.fields]
id = "/id"
"#,
    );
    let error = parse(&text).unwrap_err().to_string();
    assert!(error.contains("unknown kind"), "{error}");
    assert!(error.contains("internal"), "{error}");
}

#[test]
fn a_zero_thread_count_is_clamped_rather_than_fatal() {
    let text = document("").replace("[bridge]", "[bridge]\nhttp_threads = 0\nworker_threads = 0");
    let config = parse(&text).unwrap();
    assert_eq!(config.http_threads, 1);
    assert_eq!(config.worker_threads, 1);
}

#[test]
fn the_lease_never_undercuts_the_backoff_window() {
    let text = document("").replace(
        "[platform.forgejo]",
        "[bridge.worker]\nbackoff_max_ms = 600000\n\n[platform.forgejo]",
    );
    let config = parse(&text).unwrap();
    assert!(config.worker.lease > config.worker.backoff_max);
}

#[test]
fn project_mirroring_is_off_unless_a_mapping_asks_for_it() {
    // The switch is opt-in: an existing config mirrors issues only, and a Project
    // event stays inert exactly as it did before projects were modelled.
    let config = parse(&document("")).expect("the document loads");
    assert!(
        !config.mappings[0].sync_projects,
        "projects must be off by default"
    );

    let config = parse(&document("sync_projects = true")).expect("the document loads");
    assert!(config.mappings[0].sync_projects);
    let linear = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "linear")
        .unwrap();
    let forgejo = config
        .platforms
        .iter()
        .find(|platform| platform.name.as_str() == "forgejo")
        .unwrap();
    // And it reaches the policy the reconciler runs with, the same way the issue
    // switch does.
    assert!(config.mappings[0].policy(linear, forgejo).sync_projects);
}

#[test]
fn a_route_table_is_refused() {
    // Placement is the `[[mapping.project]]` table now; a `[[mapping.route]]` is no
    // longer a config key, so a config that still carries one fails loudly rather than
    // silently placing everything in the mapping's own scope.
    let error = parse(&document(
        "\n[[mapping.route]]\nproject = \"project-uuid\"\nscope = \"Vedaru/kuro\"\n",
    ))
    .expect_err("a route table is no longer a config key")
    .to_string();
    assert!(error.contains("route"), "{error}");
}

#[test]
fn a_project_entry_reaches_the_mapping() {
    // Placement is configuration: an entry names the project and the repository its
    // mirror lives in, and several entries may name the same repository.
    let config = parse(&document(concat!(
        "\n[[mapping.project]]\nproject = \"kuro\"\nscope = \"Vedaru/kuro\"\n",
        "\n[[mapping.project]]\nproject = \"aoe-pipelets\"\nscope = \"Vedaru/kuro\"\n",
    )))
    .expect("the config loads");
    assert_eq!(config.mappings[0].project.len(), 2);
    assert_eq!(config.mappings[0].project[0].project, "kuro");
    assert_eq!(config.mappings[0].project[0].scope, "Vedaru/kuro");

    // And it reaches the mapping the reconciler runs with, by the name a person wrote.
    let mappings = config.reconcile_mappings().expect("the mapping resolves");
    let scopes = &mappings[0].project_scopes;
    assert_eq!(scopes.len(), 2);
    assert_eq!(
        scopes.scope(&crate::reconcile::Identity {
            id: "a-uuid",
            slug: None,
            name: Some("kuro"),
        }),
        Some("Vedaru/kuro")
    );
}

#[test]
fn a_project_entry_needs_both_keys() {
    let error = parse(&document("\n[[mapping.project]]\nproject = \"kuro\"\n"))
        .expect_err("a scope is required")
        .to_string();
    assert!(error.contains("names no scope"), "{error}");

    let error = parse(&document("\n[[mapping.project]]\nscope = \"Vedaru/kuro\"\n"))
        .expect_err("a project is required")
        .to_string();
    assert!(error.contains("names no project"), "{error}");
}
