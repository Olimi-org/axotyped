//! Regression tests for generator robustness fixes: synthetic parameter-name
//! allocation, reserved-word rejection, and method-name emission.

use axotyped::{
    GeneratorConfig, HttpMethod, RouteCollection, RouteDefinition, Visibility, extract_path_params,
    generate,
};

fn route(name: &str, path: &str) -> RouteDefinition {
    RouteDefinition {
        name: name.into(),
        method: HttpMethod::Get,
        path: path.into(),
        visibility: Visibility::Public,
        declared: Visibility::Public,
        body_type: None,
        response_type: None,
        query_type: None,
        path_params: extract_path_params(path),
        group: None,
        allow_redirects: false,
        redirect: false,
        websocket: false,
        ws_send_type: None,
        ws_receive_type: None,
    }
}

/// Synthetic names must never collide with a legitimate raw parameter that
/// happens to look synthetic (`{__param_1}`), nor with each other.
#[test]
fn synthetic_names_are_collision_free() {
    let params = extract_path_params("/x/{a;evil}/{b;evil}/{__param_1}");
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names[0], "__param_0");
    // __param_1 is claimed by the third (raw, valid) parameter...
    assert_eq!(names[2], "__param_1");
    // ...so the second synthetic must skip to __param_2.
    assert_eq!(names[1], "__param_2");
    assert!(
        names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == names.len(),
        "duplicate generated parameter names"
    );
}

/// A raw parameter named exactly like a synthetic candidate keeps its name,
/// and later synthetics skip past it.
#[test]
fn synthetic_allocation_skips_raw_synthetic_lookalike() {
    let params = extract_path_params("/x/{a;b}/{__param_0}");
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names[1], "__param_0"); // valid identifier, kept as-is
    assert_eq!(names[0], "__param_1"); // first synthetic skips the taken name
}

/// Strict-mode and contextual keywords must not be emitted as parameter
/// names; they become synthetics instead.
#[test]
fn strict_mode_keywords_become_synthetic_names() {
    for keyword in [
        "let",
        "static",
        "interface",
        "package",
        "implements",
        "await",
    ] {
        let params = extract_path_params(&format!("/x/{{{keyword}}}"));
        assert_eq!(
            params[0].name, "__param_0",
            "'{keyword}' must not be used as a parameter name"
        );
    }
}

/// Method names that arrive as free-form strings are emitted as quoted,
/// escaped property keys — their content stays inert string data (a property
/// key, not code). Note the payload text still appears *inside* the quoted
/// key; what matters is that it cannot terminate the key or execute.
#[test]
fn non_identifier_method_names_are_emitted_as_quoted_keys() {
    let mut routes = RouteCollection::new();
    routes.push(route(
        "ok\"; export function pwn(){fetch('https://evil')}; x\": ",
        "/ok",
    ));
    let out = generate(&routes, &GeneratorConfig::default());

    // The line must be a single quoted key ending at `": () =>` — i.e. every
    // interior double-quote is escaped, so nothing inside can close the key.
    let line = out
        .lines()
        .find(|l| l.contains("pwn") && l.contains("=>"))
        .expect("method should be emitted");
    assert!(
        line.trim_start().starts_with('"') && line.contains("\\\"; export function pwn()"),
        "interior quotes must be escaped so the name stays one inert key: {line:?}"
    );
    // The method line is `<quoted key>: () => transport.execute<void>(OPERATIONS[<same quoted key>]),`
    // — the key terminates at its own closing quote in both positions.
    let closer = "\": () => transport.execute<void>(OPERATIONS[\"";
    assert!(
        line.contains(closer) && line.trim_end().ends_with("\"]),"),
        "the emitted key must terminate exactly at its own closing quote: {line:?}"
    );
}

/// A hand-built untyped WS definition (the builder path makes this
/// unrepresentable) fails generation — no contract, no method.
#[test]
#[should_panic(expected = "without declared event types")]
fn untyped_websocket_route_fails_generation() {
    let mut routes = RouteCollection::new();
    let mut def = route("rawSocket", "/raw");
    def.websocket = true;
    routes.push(def);
    routes.push(route("health", "/health"));

    let _ = generate(&routes, &GeneratorConfig::default());
}

/// The flat emitter shares the contract gate: with grouping disabled an
/// untyped WS route still fails instead of producing a method referencing
/// a descriptor the table skipped.
#[test]
#[should_panic(expected = "without declared event types")]
fn untyped_websocket_route_fails_generation_without_grouping() {
    let mut routes = RouteCollection::new();
    let mut def = route("rawSocket", "/raw");
    def.websocket = true;
    routes.push(def);
    routes.push(route("health", "/health"));

    let config = GeneratorConfig {
        enable_groups: false,
        ..Default::default()
    };
    let _ = generate(&routes, &config);
}

/// A partial contract (send declared, receive missing) is no contract: one
/// predicate gates generation, the descriptor table, and both emitters.
#[test]
#[should_panic(expected = "without declared event types")]
fn partial_websocket_contract_fails_generation() {
    let mut routes = RouteCollection::new();
    let mut def = route("halfTyped", "/half");
    def.websocket = true;
    def.ws_send_type = Some("ClientEvent".into());
    routes.push(def);
    routes.push(route("health", "/health"));

    let _ = generate(&routes, &GeneratorConfig::default());
}

/// Ordinary derived identifiers still emit bare (no quotes), so the common
/// case produces unchanged output.
#[test]
fn identifier_method_names_stay_bare() {
    let mut routes = RouteCollection::new();
    routes.push(route("listUsers", "/users"));
    let out = generate(&routes, &GeneratorConfig::default());
    assert!(out.contains("listUsers: () =>"));
}

/// The withOptions collision check follows the layout it is checking:
/// flat emission puts every route method at the top level (a grouped route
/// named after a helper collides; a group name creates no property), while
/// grouped nesting exposes the group object there instead.
#[test]
#[should_panic(expected = "built-in")]
fn helper_collision_in_flat_layout_fails_generation() {
    let mut routes = RouteCollection::new();
    let mut grouped = route("withOptions", "/derive");
    grouped.group = Some("admin".into());
    routes.push(grouped);

    let flat = GeneratorConfig {
        enable_groups: false,
        ..Default::default()
    };
    let _ = generate(&routes, &flat);
}

#[test]
fn helper_collision_absent_in_grouped_layout_generates() {
    let mut routes = RouteCollection::new();
    let mut grouped = route("withOptions", "/derive");
    grouped.group = Some("admin".into());
    routes.push(grouped);

    let out = generate(&routes, &GeneratorConfig::default());
    assert!(
        out.contains("withOptions:"),
        "grouped layout: the method nests under 'admin', no collision: {out}"
    );
}

#[test]
#[should_panic(expected = "built-in")]
fn group_named_after_helper_fails_generation() {
    let mut routes = RouteCollection::new();
    let mut listed = route("list", "/list");
    listed.group = Some("withOptions".into());
    routes.push(listed);

    let _ = generate(&routes, &GeneratorConfig::default());
}

#[test]
fn group_name_creates_no_property_in_flat_layout() {
    let mut routes = RouteCollection::new();
    let mut listed = route("list", "/list");
    listed.group = Some("withOptions".into());
    routes.push(listed);

    let flat = GeneratorConfig {
        enable_groups: false,
        ..Default::default()
    };
    let out = generate(&routes, &flat);
    assert!(
        out.contains("list:"),
        "flat layout: a group name creates no property, 'list' is the method: {out}"
    );
}
