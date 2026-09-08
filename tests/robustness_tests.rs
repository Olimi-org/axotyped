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
        .find(|l| l.contains("pwn"))
        .expect("method should be emitted");
    assert!(
        line.trim_start().starts_with('"') && line.contains("\\\"; export function pwn()"),
        "interior quotes must be escaped so the name stays one inert key: {line:?}"
    );
    assert!(
        line.trim_end()
            .ends_with("\": () => request<void>(\"/ok\", { method: \"GET\", permissive: false, allowRedirects: false }),"),
        "the emitted key must terminate exactly at its own closing quote"
    );
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
