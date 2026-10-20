use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;
use std::path::{Component, Path};
use std::process::Command;

use crate::types::{RouteCollection, RouteDefinition, Visibility};

/// Shorthand for `writeln!(...).unwrap()` — writing to `String` is infallible.
macro_rules! w {
    ($dst:expr) => { writeln!($dst).unwrap() };
    ($dst:expr, $($arg:tt)*) => { writeln!($dst, $($arg)*).unwrap() };
}

/// How the generated client authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// `Authorization` header via `ClientOptions.getToken`; auth routes throw without a token.
    Bearer,
    /// Session cookies (automatic); refuses `credentials: "omit"` on auth routes.
    Cookie,
    /// No auth machinery; auth routes emit diagnostics.
    None,
}

/// Configuration for the TypeScript client generator.
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// Directory where ts-rs generates type bindings (e.g., `"./bindings"`).
    pub bindings_dir: String,
    /// Output path for the generated client file.
    pub output_path: String,
    /// Name of the factory function (e.g., `"createYAuthClient"`).
    pub factory_name: String,
    /// Whether to generate grouped nested objects.
    pub enable_groups: bool,
    /// Name of the error class (e.g., `"ApiError"` or `"YAuthError"`).
    pub error_class_name: String,
    /// Name of the options interface (e.g., `"ClientOptions"` or `"YAuthClientOptions"`).
    pub options_interface_name: String,
    /// Default `RequestCredentials` (default `"same-origin"`).
    pub default_credentials: String,
    /// Type import prefix; if empty, computed from `bindings_dir` vs `output_path`.
    pub type_import_prefix: String,
    /// Optional formatter command; output path is appended as last arg.
    pub format_command: Option<String>,
    /// TS type for integers beyond JS safe range (`u64`/`i64`/`u128`/`i128`/`usize`/`isize`).
    /// Default `"number"` (loses precision above 2^53); `"bigint"` or `"string"` preserve it.
    pub large_int_type: String,
    /// Auth model of the generated client (default [`AuthScheme::Bearer`]).
    pub auth_scheme: AuthScheme,
    /// CSRF header for mutating requests under [`AuthScheme::Cookie`].
    /// `None` emits no CSRF plumbing. Ignored under other schemes.
    pub csrf_header_name: Option<String>,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            bindings_dir: "./bindings".into(),
            output_path: "./generated.ts".into(),
            factory_name: "createApiClient".into(),
            enable_groups: true,
            error_class_name: "ApiError".into(),
            options_interface_name: "ClientOptions".into(),
            default_credentials: "same-origin".into(),
            type_import_prefix: String::new(),
            format_command: None,
            large_int_type: "number".into(),
            auth_scheme: AuthScheme::Bearer,
            csrf_header_name: None,
        }
    }
}

impl GeneratorConfig {
    /// Returns a ts-rs [`Config`](crate::ts::Config) with matching `large_int_type`,
    /// for use with [`RouteCollection::export_types_with`](crate::RouteCollection::export_types_with).
    pub fn ts_config(&self) -> crate::ts::Config {
        crate::ts::Config::from_env().with_large_int(self.large_int_type.clone())
    }
}

/// Error returned by the check function when generated output doesn't match committed file.
#[derive(Debug)]
pub enum CheckError {
    /// Generated output differs from the committed file.
    OutOfSync { path: String },
    /// Could not read the committed file.
    ReadError { path: String, error: std::io::Error },
    /// Generation itself failed.
    GenerateError(String),
    /// The format command failed to execute.
    FormatError {
        command: String,
        error: std::io::Error,
    },
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfSync { path } => {
                write!(
                    f,
                    "Generated TypeScript client is out of sync with '{path}'. \
                     Run the generate command to update it."
                )
            }
            Self::ReadError { path, error } => {
                write!(f, "Failed to read '{path}': {error}")
            }
            Self::GenerateError(msg) => write!(f, "Generation error: {msg}"),
            Self::FormatError { command, error } => {
                write!(f, "Format command '{command}' failed: {error}")
            }
        }
    }
}

impl std::error::Error for CheckError {}

// ---------------------------------------------------------------------------
// Type helpers
// ---------------------------------------------------------------------------

const PRIMITIVES: &[&str] = &[
    "String", "&str", "Uuid", "bool", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32",
    "f64", "usize", "isize", "u128", "i128",
];

/// Integer types rendered via the configured `large_int_type`.
const LARGE_INT_TYPES: &[&str] = &["u64", "i64", "u128", "i128", "usize", "isize"];

/// Stdlib containers handled inline without TS imports.
const CONTAINERS: &[&str] = &["Vec", "Option"];

/// Names this generator injects onto the client object alongside the route
/// methods. A route that claims one of them would be overwritten by
/// `Object.assign`, so they are reserved by name rather than by mangling —
/// the closed world is small enough to state, and the check that reads it
/// lives next to the code that emits these.
const CLIENT_HELPERS: &[&str] = &["withOptions"];

/// Strips `Vec`/`Option` wrappers to the innermost type.
#[cfg(test)]
fn unwrap_inner(rust_type: &str) -> &str {
    let t = rust_type.trim();
    if let Some(inner) = t
        .strip_prefix("Vec<")
        .or_else(|| t.strip_prefix("Option<"))
        .and_then(|s| s.strip_suffix('>'))
    {
        return unwrap_inner(inner);
    }
    t
}

/// Converts a Rust type to TS, mapping wide integers via `large_int`.
fn rust_type_to_ts_with(rust_type: &str, large_int: &str) -> String {
    let t = rust_type.trim();
    if let Some(inner) = t.strip_prefix("Vec<").and_then(|s| s.strip_suffix('>')) {
        return format!("{}[]", rust_type_to_ts_with(inner, large_int));
    }
    if let Some(inner) = t.strip_prefix("Option<").and_then(|s| s.strip_suffix('>')) {
        return format!("{} | null", rust_type_to_ts_with(inner, large_int));
    }
    match t {
        "String" | "&str" | "Uuid" => "string".into(),
        "bool" => "boolean".into(),
        _ if LARGE_INT_TYPES.contains(&t) => large_int.to_string(),
        _ if PRIMITIVES.contains(&t) => "number".into(),
        _ => t.to_string(),
    }
}

/// Converts a Rust type to TS with default mappings.
#[cfg(test)]
fn rust_type_to_ts(rust_type: &str) -> String {
    rust_type_to_ts_with(rust_type, "bigint")
}

/// Returns true for primitives needing no import.
#[cfg(test)]
fn is_primitive_type(rust_type: &str) -> bool {
    PRIMITIVES.contains(&unwrap_inner(rust_type))
}

/// Collects custom type names for imports.
/// e.g. `Vec<User>` -> `["User"]`; `ContentResponse<Dialog>` -> `["ContentResponse", "Dialog"]`.
fn extract_type_names(rust_type: &str) -> Vec<&str> {
    let mut names = Vec::new();
    collect_type_names(rust_type, &mut names);
    names.sort();
    names.dedup();
    names
}

fn collect_type_names<'a>(t: &'a str, out: &mut Vec<&'a str>) {
    let t = t.trim();

    // Strip Vec<>/Option<> wrappers — recurse into inner type only
    if let Some(inner) = t
        .strip_prefix("Vec<")
        .or_else(|| t.strip_prefix("Option<"))
        .and_then(|s| s.strip_suffix('>'))
    {
        collect_type_names(inner, out);
        return;
    }

    // Check for generic: Name<Inner1, Inner2, ...>
    if let Some(inner) = t.strip_suffix('>').and_then(|s| s.split_once('<')) {
        let (name, params) = inner;
        let name = name.trim();
        // Skip Rust stdlib containers (Vec, Option, Result, etc.)
        if !PRIMITIVES.contains(&name) && !CONTAINERS.contains(&name) {
            out.push(name);
        }
        // Recurse into each generic parameter, splitting on commas
        // while respecting angle bracket nesting depth.
        for param in split_generic_params(params) {
            collect_type_names(param, out);
        }
        return;
    }

    // Plain type
    if !PRIMITIVES.contains(&t) && !CONTAINERS.contains(&t) {
        out.push(t);
    }
}

/// Splits generic params on top-level commas.
/// e.g. `"ContentResponse<Dialog>, ApiError"` -> `["ContentResponse<Dialog>", " ApiError"]`.
fn split_generic_params(params: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (i, ch) in params.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                result.push(&params[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < params.len() {
        result.push(&params[start..]);
    }
    result
}

// ---------------------------------------------------------------------------
// Path and parameter helpers
// ---------------------------------------------------------------------------

/// Computes the type-import prefix from config or relative paths.
fn compute_import_prefix(config: &GeneratorConfig) -> String {
    if !config.type_import_prefix.is_empty() {
        return config.type_import_prefix.clone();
    }

    let output_dir = Path::new(&config.output_path)
        .parent()
        .unwrap_or(Path::new("."));
    let bindings = Path::new(&config.bindings_dir);

    fn normal_parts(p: &Path) -> Vec<&str> {
        p.components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect()
    }

    let out_parts = normal_parts(output_dir);
    let bind_parts = normal_parts(bindings);

    let common = out_parts
        .iter()
        .zip(&bind_parts)
        .take_while(|(a, b)| a == b)
        .count();

    let ups = out_parts.len() - common;
    let mut prefix = if ups == 0 {
        ".".to_string()
    } else {
        "../".repeat(ups)
    };
    let suffix = bind_parts[common..].join("/");
    if !suffix.is_empty() {
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        prefix.push_str(&suffix);
    }
    prefix
}

/// Escapes text for a double-quoted JS string. `$` is escaped: callers
/// re-embed the text in template literals.
pub(crate) fn escape_js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '`' => out.push_str("\\`"),
            // Template-literal safety.
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Emit a route name as an object key.
fn emit_method_name(out: &mut String, name: &str) {
    if crate::types::is_valid_js_identifier(name) {
        out.push_str(name);
    } else {
        out.push('"');
        out.push_str(&escape_js_string(name));
        out.push('"');
    }
}

/// Property access for a *wire* field name — the key as it appears in the
/// server's JSON, which is not always a JS identifier.
fn wire_access(out: &mut String, field: &str) {
    if crate::types::is_valid_js_identifier(field) {
        out.push('.');
        out.push_str(field);
    } else {
        out.push_str("[\"");
        out.push_str(&escape_js_string(field));
        out.push_str("\"]");
    }
}

/// Emit the typed renewal helper a declared flow buys.
///
/// This is where the flow declaration's field names are actually spent:
/// the generated file reads them off the *imported response type*, so the
/// compiler checks every name against the type the client will parse. The
/// runtime used to do this same extraction against a name it was handed as
/// a string, which nothing could check.
///
/// What the helper does not do is decide anything — it returns a value and
/// stops. Storing it, scheduling it, and deciding what a lapse means are
/// the application's.
fn refresh_helper(
    flow: &crate::auth_flow::AuthFlow,
    routes: &RouteCollection,
    type_name: &str,
    enable_groups: bool,
) -> Option<String> {
    // A lifecycle route that cannot be called back with no arguments has
    // no helper to generate; the warning stream already says so.
    if default_hook_blocker(flow, routes).is_some() {
        return None;
    }
    let fields = flow.token_fields.as_ref()?;
    let route_name = flow
        .refresh
        .as_deref()
        .or_else(|| flow.bootstrap.first().map(|s| s.as_str()))?;
    let route = routes.find_by_name(route_name)?;

    let mut s = String::new();
    // Call through the typed route method so the response keeps its type —
    // that type is what validates the field names below.
    // Expression-position access: `.name` for identifiers, `["name"]`
    // otherwise — emit_method_name would quote a literal key, which is not
    // valid after `client.`.
    let mut call = String::from("client");
    match &route.group {
        // With grouping disabled the flat emitter puts every route method
        // at the top level — the group segment only exists when it's
        // nested, or the helper calls a property the client does not have.
        Some(group) if enable_groups => wire_access(&mut call, group),
        _ => {}
    }
    wire_access(&mut call, &route.name);
    call.push_str("()");

    match fields {
        crate::auth_flow::TokenFields::Bearer(field) => {
            let mut read = String::from("res");
            wire_access(&mut read, field);
            w!(
                s,
                "/** Renews the bearer via `{name}` and returns the token, or null when\n\
                 \x20 * the response carries none. Hand it to `onUnauthorized`, or call it on\n\
                 \x20 * your own schedule using `jwtExpiry` to decide when. */",
                name = route.name
            );
            w!(
                s,
                "export async function refreshBearer(client: {ty}): Promise<string | null> {{",
                ty = type_name
            );
            w!(s, "  const res = await {call};", call = call);
            w!(s, "  return {read} ?? null;", read = read);
            w!(s, "}}");
        }
        crate::auth_flow::TokenFields::Pair {
            access,
            refresh,
            expires_in,
        } => {
            let mut a = String::from("res");
            wire_access(&mut a, access);
            // An undeclared optional member is a literal null, not a read
            // that happens to be undefined — the return type promises null.
            let r = match refresh.as_ref() {
                Some(f) => {
                    let mut acc = String::from("res");
                    wire_access(&mut acc, f);
                    format!("{acc} ?? null")
                }
                None => "null".to_string(),
            };
            let e = match expires_in.as_ref() {
                Some(f) => {
                    let mut acc = String::from("res");
                    wire_access(&mut acc, f);
                    format!("{acc} == null ? null : Number({acc})")
                }
                None => "null".to_string(),
            };
            w!(
                s,
                "/** Renews the credential via `{name}` and lifts the declared fields off\n\
                 \x20 * the response. `refresh` and `expiresIn` are null when the server omits\n\
                 \x20 * them — with no `expiresIn` declared you may derive one from\n\
                 \x20 * `jwtExpiry(access)`. */",
                name = route.name
            );
            w!(
                s,
                "export async function refreshSession(client: {ty}): Promise<{{\n\
                 \x20 access: string; refresh: string | null; expiresIn: number | null;\n\
                 \x20 }}> {{",
                ty = type_name
            );
            w!(s, "  const res = await {call};", call = call);
            w!(s, "  return {{");
            w!(s, "    access: {a},", a = a);
            w!(s, "    refresh: {r},", r = r);
            w!(s, "    expiresIn: {e},", e = e);
            w!(s, "  }};");
            w!(s, "}}");
        }
    }
    w!(s);
    Some(s)
}

/// Generates the parameter list for a route.
///
/// Path params are emitted under collision-proof JS names: a param named
/// `transport` would otherwise shadow the `transport: Transport` closure
/// binding (`(transport: string) => transport.execute(...)`), `OPERATIONS`
/// would shadow the route table, and `body` / `query` would duplicate the
/// body/query params. [`js_param_names`] maps wire names to safe locals;
/// this renders the local side.
fn generate_params(route: &RouteDefinition, large_int: &str) -> String {
    let mapping = js_param_names(route);
    let mut params = Vec::new();
    for (_, js) in &mapping {
        params.push(format!("{js}: string"));
    }
    if let Some(ref body_type) = route.body_type {
        params.push(format!(
            "body: {}",
            rust_type_to_ts_with(body_type, large_int)
        ));
    }
    if let Some(ref query_type) = route.query_type {
        params.push(format!(
            "query?: {}",
            rust_type_to_ts_with(query_type, large_int)
        ));
    }
    params.join(", ")
}

// ---------------------------------------------------------------------------
// Template constants for static TypeScript blocks
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Client options + shared runtime emission
// ---------------------------------------------------------------------------

const INSECURE_HTTP_DOC: &str = r#"  /** Allow non-loopback http:// targets (development only). */
  allowInsecureHttp?: boolean;
"#;

/// ClientOptions fields shared by every scheme.
///
/// Runtime types are referenced through their `__Axo*` aliases (see the
/// runtime import block in `generate_client`): a user DTO named e.g.
/// `Transport` or `OperationSpec` would otherwise collide with the runtime
/// import of the same name.
const OPTIONS_TAIL: &str = r#"  credentials?: RequestCredentials;
  fetch?: typeof fetch;
  onError?: (error: __AxoApiError) => void;
  onRequest?: (op: Readonly<__AxoOperationSpec>, url: string) => void;
  onResponse?: (op: Readonly<__AxoOperationSpec>, response: Response) => void;
  requestInit?: Omit<RequestInit, "headers" | "method" | "body"> & {
    headers?: Record<string, string>;
  };
"#;

const OPTIONS_CSRF_FIELD: &str = r#"  /** CSRF token attached as the "__CSRF_NAME__" header on mutating requests. */
  csrfToken?: () => Promise<string | null> | string | null;
"#;

/// The shared client runtime, emitted beside the client file.
pub fn generate_runtime_source() -> String {
    include_str!("runtime.ts").replace("__RUNTIME_VERSION__", env!("CARGO_PKG_VERSION"))
}

/// File stem of the runtime module.
fn runtime_file_stem(config: &GeneratorConfig) -> String {
    let stem = Path::new(&config.output_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("client");
    format!("{stem}-runtime")
}

/// Path the runtime module is written to.
fn runtime_output_path(config: &GeneratorConfig) -> std::path::PathBuf {
    Path::new(&config.output_path)
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}.ts", runtime_file_stem(config)))
}

/// Import specifier the client uses for the runtime module.
fn runtime_import(config: &GeneratorConfig) -> String {
    format!("./{}", escape_js_string(&runtime_file_stem(config)))
}

fn scheme_label(config: &GeneratorConfig) -> &'static str {
    match config.auth_scheme {
        AuthScheme::Bearer => "bearer",
        AuthScheme::Cookie => "cookie",
        AuthScheme::None => "none",
    }
}

/// Assemble the ClientOptions interface. A declared flow additionally
/// exposes `onUnauthorized`, the renewal hook the transport consults after
/// a 401; without a flow the application renews on its own schedule and
/// only names where the credential lives.
fn options_interface(config: &GeneratorConfig, lifecycle: bool) -> String {
    let mut s = String::from("export interface __OPTS__ {\n  baseUrl: string;\n");
    match config.auth_scheme {
        AuthScheme::Bearer => {
            s.push_str(
                "  /** The bearer source — where the credential lives. Consulted on every\n\
                 \x20     credentialed call. */\n\
                 \x20 getToken?: () => Promise<string | null> | string | null;\n",
            );
            if lifecycle {
                s.push_str(
                    "  /** Renewal policy. Called after a 401 on a required route; return the\n\
                     \x20     credential to replay with, or null when the session cannot be\n\
                     \x20     renewed. Throwing is treated as null. The replay rides this\n\
                     \x20     return value — the transport never re-reads getToken to decide who\n\
                     \x20     the caller is, so a request cannot be re-sent under a session you\n\
                     \x20     have since replaced. */\n\
                     \x20 onUnauthorized?: () => Promise<string | null>;\n",
                );
            }
        }
        AuthScheme::Cookie => {
            if config.csrf_header_name.is_some() {
                s.push_str(&OPTIONS_CSRF_FIELD.replace(
                    "__CSRF_NAME__",
                    &escape_js_string(config.csrf_header_name.as_deref().unwrap_or("X-CSRF-Token")),
                ));
            }
        }
        AuthScheme::None => {}
    }
    s.push_str(OPTIONS_TAIL);
    s.push_str(INSECURE_HTTP_DOC);
    s.push_str("}\n");
    s.replace("__OPTS__", &config.options_interface_name)
}

/// Credential requirement for the OPERATIONS table.
fn auth_mode(route: &RouteDefinition) -> &'static str {
    match route.visibility {
        Visibility::Private => "required",
        Visibility::Permissive => "optional",
        Visibility::Public => "none",
    }
}

fn response_mode(route: &RouteDefinition) -> &'static str {
    if route.redirect || route.response_type.is_none() {
        "void"
    } else {
        "json"
    }
}

/// Whether a WebSocket route carries both event types.
fn has_ws_contract(route: &RouteDefinition) -> bool {
    route.ws_send_type.is_some() && route.ws_receive_type.is_some()
}

/// Rewrites a route path for the OPERATIONS table.
fn safe_spec_path(path: &str, params: &[crate::types::PathParam]) -> String {
    if !path.contains('{') {
        return escape_js_string(path);
    }
    let mut out = String::new();
    let mut rest = path;
    let mut seen: Vec<&str> = Vec::new();
    loop {
        match rest.find('{') {
            None => {
                out.push_str(&escape_js_string(rest));
                break;
            }
            Some(open) => {
                out.push_str(&escape_js_string(&rest[..open]));
                let after = &rest[open..];
                match after[1..].find('}') {
                    None => {
                        out.push_str(&escape_js_string(after));
                        break;
                    }
                    Some(close_rel) => {
                        let raw = &after[1..1 + close_rel];
                        let idx = match seen.iter().position(|r| *r == raw) {
                            Some(i) => i,
                            None => {
                                seen.push(raw);
                                seen.len() - 1
                            }
                        };
                        let safe = params.get(idx).map(|p| p.name.as_str()).unwrap_or(raw);
                        out.push('{');
                        out.push_str(safe);
                        out.push('}');
                        rest = &after[1 + close_rel + 1..];
                    }
                }
            }
        }
    }
    out
}

/// Route name as an OPERATIONS-table key (quoted when not an identifier).
fn op_key(key: &str) -> String {
    if crate::types::is_valid_js_identifier(key) {
        key.to_string()
    } else {
        format!("\"{}\"", escape_js_string(key))
    }
}

/// Property reference into OPERATIONS for a table key.
fn op_ref(key: &str) -> String {
    if crate::types::is_valid_js_identifier(key) {
        format!(".{key}")
    } else {
        format!("[\"{}\"]", escape_js_string(key))
    }
}

/// Unique OPERATIONS-table key per route.
fn operations_keys(routes: &RouteCollection) -> HashMap<(Option<String>, String), String> {
    let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut map = HashMap::new();
    for route in routes.iter() {
        let base = match &route.group {
            Some(group) => format!("{}.{}", group, route.name),
            None => route.name.clone(),
        };
        let mut key = base.clone();
        let mut n = 2;
        while !used.insert(key.clone()) {
            key = format!("{base}#{n}");
            n += 1;
        }
        map.insert((route.group.clone(), route.name.clone()), key);
    }
    map
}

fn key_of(keys: &HashMap<(Option<String>, String), String>, route: &RouteDefinition) -> String {
    keys.get(&(route.group.clone(), route.name.clone()))
        .cloned()
        .unwrap_or_else(|| route.name.clone())
}

/// JS-local names claimed by the generated route-method scope.
///
/// A route method closes over the factory's `transport` binding and the
/// module-level `OPERATIONS` table, and declares `body` / `query` params of
/// its own — a path param with any of those names would shadow or duplicate
/// them (`/items/{transport}` used to emit `(transport: string) =>
/// transport.execute(...)`). Wire names stay untouched; only locals are
/// mangled, with `params: { wire: local }` mapping in [`call_args`].
fn reserved_param_names() -> &'static [&'static str] {
    &["transport", "TRANSPORT", "OPERATIONS", "body", "query"]
}

/// Maps each path param wire name to its JS-local name.
///
/// Returns `(wire_name, js_name)` pairs in path order. Names colliding with
/// [`reserved_param_names`] (or with each other after mangling) gain trailing
/// underscores until unique.
fn js_param_names(route: &RouteDefinition) -> Vec<(String, String)> {
    let reserved = reserved_param_names();
    let mut used: BTreeSet<String> = BTreeSet::new();
    route
        .path_params
        .iter()
        .map(|p| {
            let wire = p.name.clone();
            let mut candidate = wire.clone();
            if reserved.contains(&candidate.as_str()) {
                candidate.push('_');
            }
            while !used.insert(candidate.clone()) {
                candidate.push('_');
            }
            (wire, candidate)
        })
        .collect()
}

/// The per-call argument bag a route method forwards to the executor.
fn call_args(route: &RouteDefinition) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !route.path_params.is_empty() {
        let mapping = js_param_names(route);
        let names = mapping
            .iter()
            .map(|(wire, js)| {
                if wire == js {
                    js.clone()
                } else {
                    format!("{wire}: {js}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("params: {{ {names} }}"));
    }
    if route.body_type.is_some() {
        parts.push("body".into());
    }
    if route.query_type.is_some() {
        parts.push("query".into());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(", {{ {} }}", parts.join(", "))
    }
}

/// Emit the OPERATIONS descriptor table.
fn write_operations_table(
    out: &mut String,
    routes: &RouteCollection,
    keys: &HashMap<(Option<String>, String), String>,
    // Ticket mint descriptor parses JSON even without a response type.
    ticket_key: Option<&str>,
) {
    w!(out, "/** Route table. */");
    w!(out, "export const OPERATIONS = {{");
    for route in routes.iter() {
        if route.websocket && !has_ws_contract(route) {
            continue;
        }
        let mut fields = vec![
            format!("method: \"{}\"", route.method.as_str()),
            format!(
                "path: \"{}\"",
                safe_spec_path(&route.path, &route.path_params)
            ),
        ];
        if !route.path_params.is_empty() {
            let names = route
                .path_params
                .iter()
                .map(|p| format!("\"{}\"", p.name))
                .collect::<Vec<_>>()
                .join(", ");
            fields.push(format!("params: [{names}]"));
        }
        fields.push(format!("auth: \"{}\"", auth_mode(route)));
        if route.body_type.is_some() {
            fields.push("body: \"json\"".into());
        }
        let key = key_of(keys, route);
        let response = if ticket_key == Some(key.as_str()) {
            "json"
        } else {
            response_mode(route)
        };
        fields.push(format!("response: \"{}\"", response));
        if route.allow_redirects {
            fields.push("allowRedirects: true".into());
        }
        if route.websocket {
            fields.push("ws: true".into());
        }
        w!(out, "  {}: {{ {} }},", op_key(&key), fields.join(", "));
    }
    w!(out, "}} satisfies Record<string, __AxoOperationSpec>;");
    w!(out);
}

/// Emit the AUTH strategy — the declaration as data. The runtime does not
/// read it; `refreshBearer`/`refreshSession` is generated from the same
/// fields and is the part you call. `skewMs` and `expiryFromJwt` are
/// advisory hints for the renewal code you own.
fn auth_strategy_const(
    flow: &crate::auth_flow::AuthFlow,
    routes: &RouteCollection,
    keys: &HashMap<(Option<String>, String), String>,
) -> String {
    let mut s = String::new();
    w!(
        s,
        "/** Credential lifecycle, as declared server-side. Read by your own renewal\n\
         \x20 * code; `refreshBearer` below is generated from the same fields. */"
    );
    w!(s, "export const AUTH = {{");
    // Lifecycle references name routes; the table keys qualify them.
    let named = |name: &str| -> Option<String> {
        routes
            .find_by_name(name)
            .map(|r| key_of(keys, r).to_string())
    };
    let refresh_key = flow
        .refresh
        .as_deref()
        .or_else(|| flow.bootstrap.first().map(|s| s.as_str()))
        .and_then(named);
    let default_hook_blocked = default_hook_blocker(flow, routes).is_some();
    match flow.kind {
        crate::auth_flow::AuthFlowKind::StaticBearer => {
            w!(s, "  kind: \"static_bearer\",");
        }
        crate::auth_flow::AuthFlowKind::Exchange => {
            w!(s, "  kind: \"exchange\",");
            match refresh_key {
                Some(k) => w!(s, "  refresh: \"{}\",", escape_js_string(&k)),
                None => w!(s, "  refresh: \"\","),
            }
            if !default_hook_blocked {
                match &flow.token_fields {
                    Some(crate::auth_flow::TokenFields::Bearer(field)) => {
                        w!(s, "  field: \"{}\",", escape_js_string(field));
                        w!(s, "  expiryFromJwt: true,");
                    }
                    _ => {}
                }
            }
            w!(s, "  skewMs: 30_000,");
        }
        crate::auth_flow::AuthFlowKind::OAuth2 => {
            w!(s, "  kind: \"oauth2\",");
            match refresh_key {
                Some(k) => w!(s, "  refresh: \"{}\",", escape_js_string(&k)),
                None => w!(s, "  refresh: \"\","),
            }
            if !default_hook_blocked {
                match &flow.token_fields {
                    Some(crate::auth_flow::TokenFields::Pair {
                        access,
                        refresh,
                        expires_in,
                    }) => {
                        w!(s, "  fields: {{");
                        w!(s, "    access: \"{}\",", escape_js_string(access));
                        if let Some(f) = refresh {
                            w!(s, "    refresh: \"{}\",", escape_js_string(f));
                        }
                        if let Some(f) = expires_in {
                            w!(s, "    expiresIn: \"{}\",", escape_js_string(f));
                        }
                        w!(s, "  }},");
                    }
                    _ => {}
                }
            }
            w!(s, "  skewMs: 30_000,");
        }
    }
    // Flow manifest.
    w!(
        s,
        "  bootstrap: [{}],",
        flow.bootstrap
            .iter()
            .filter_map(|n| named(n))
            .map(|k| format!("\"{}\"", escape_js_string(&k)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    w!(s, "  constants: {{");
    for (key, value) in &flow.constants {
        w!(
            s,
            "    \"{}\": \"{}\",",
            escape_js_string(&crate::builder::snake_to_camel(key)),
            escape_js_string(value)
        );
    }
    w!(s, "  }},");
    w!(
        s,
        "}} as const satisfies __AxoAuthStrategy & __AxoFlowManifest;"
    );
    s
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

/// Why the default refresh hook cannot bind to a flow's lifecycle route.
enum DefaultHookBlocker {
    NotRegistered,
    NotPlainHttp,
    Body,
    PathParams,
    Query,
    NoResponseType,
}

/// Whether the declared lifecycle route is callable by the generated
/// renewal helper — i.e. reachable with no arguments, as a typed response.
/// [`None`] means the helper can be generated.
fn default_hook_blocker(
    flow: &crate::auth_flow::AuthFlow,
    routes: &RouteCollection,
) -> Option<DefaultHookBlocker> {
    let route_name = flow
        .refresh
        .as_deref()
        .or_else(|| flow.bootstrap.first().map(|s| s.as_str()));
    match route_name.and_then(|name| routes.find_by_name(name)) {
        None => Some(DefaultHookBlocker::NotRegistered),
        Some(r) if r.websocket || r.redirect => Some(DefaultHookBlocker::NotPlainHttp),
        Some(r) if r.body_type.is_some() => Some(DefaultHookBlocker::Body),
        Some(r) if !r.path_params.is_empty() => Some(DefaultHookBlocker::PathParams),
        Some(r) if r.query_type.is_some() => Some(DefaultHookBlocker::Query),
        Some(r) if r.response_type.is_none() => Some(DefaultHookBlocker::NoResponseType),
        Some(_) => None,
    }
}

/// Generates client source.
///
/// Misconfigurations that would produce a broken or silently wrong client
/// fail here instead of emitting diagnostics: generation runs on every
/// codegen invocation, so a panic names the problem at the site that must
/// be fixed.
pub fn generate(routes: &RouteCollection, config: &GeneratorConfig) -> String {
    // Validate the default refresh route.
    if let Some(flow) = routes.auth_flow() {
        if config.auth_scheme == AuthScheme::Bearer && flow.token_fields.is_some() {
            let route_name = flow
                .refresh
                .as_deref()
                .or_else(|| flow.bootstrap.first().map(|s| s.as_str()));
            let remedy = "declare a callable lifecycle route or supply your own onUnauthorized";
            match default_hook_blocker(flow, routes) {
                None => {}
                Some(DefaultHookBlocker::NotRegistered) => panic!(
                    "auth flow '{}': token fields declared but their route is \
                     not registered — no renewal helper can be generated; {remedy}.",
                    flow.name
                ),
                Some(blocker) => {
                    let r = route_name
                        .and_then(|name| routes.find_by_name(name))
                        .expect("route exists");
                    panic!(
                        "auth flow '{}': the lifecycle route '{}' {}; no renewal \
                         helper can be generated; {remedy}.",
                        flow.name,
                        r.name,
                        match blocker {
                            DefaultHookBlocker::NotPlainHttp if r.websocket => {
                                "is a websocket route"
                            }
                            DefaultHookBlocker::NotPlainHttp => {
                                "is a redirect route"
                            }
                            DefaultHookBlocker::Body => {
                                "takes a request body"
                            }
                            DefaultHookBlocker::PathParams => {
                                "takes path parameters"
                            }
                            DefaultHookBlocker::Query => {
                                "takes a query string"
                            }
                            DefaultHookBlocker::NoResponseType => {
                                "declares no response type"
                            }
                            DefaultHookBlocker::NotRegistered => {
                                unreachable!("handled above")
                            }
                        }
                    );
                }
            }
        }
    }

    for route in routes.iter().filter(|r| r.websocket && !has_ws_contract(r)) {
        panic!(
            "route '{}': [ws] without declared event types — the route would \
             generate no client method. Call .events::<Send, Receive>() on it, \
             or register the raw socket outside the route table.",
            route.name
        );
    }

    // Closed-world check. The client object is the route table plus the
    // helpers this generator injects. Both are ours, so a route landing on
    // an injected name is knowable right here: `Object.assign` would
    // overwrite the property, and calling it would derive a new client
    // instead of sending the request.
    {
        let mut reported: BTreeSet<(Option<String>, String)> = BTreeSet::new();
        let mut report = |route: &RouteDefinition| {
            // Which property actually takes the helper's name depends on the
            // layout: grouped nesting exposes the GROUP at the top level,
            // the flat layout exposes every route method there.
            let (collide, property) = match &route.group {
                Some(g) if config.enable_groups => {
                    (CLIENT_HELPERS.contains(&g.as_str()), g.clone())
                }
                _ => (
                    CLIENT_HELPERS.contains(&route.name.as_str()),
                    route.name.clone(),
                ),
            };
            if collide && reported.insert((route.group.clone(), route.name.clone())) {
                panic!(
                    "route '{}'{} collides with the client's built-in `{helper}` helper — the \
                     generated property would be overwritten by it, and calling it would derive \
                     a new client instead of sending the request. Rename with .as_().",
                    route.name,
                    match &route.group {
                        None => String::new(),
                        Some(g) => format!(" in group '{g}'"),
                    },
                    helper = property,
                );
            }
        };
        for route in routes.iter() {
            report(route);
        }
        // The flow's synthetic `auth` namespace is emitted beside the groups.
        if config.auth_scheme == AuthScheme::Bearer {
            if let Some(flow) = routes.auth_flow() {
                for name in flow
                    .bootstrap
                    .iter()
                    .chain(flow.refresh.iter())
                    .chain(flow.ws_ticket.iter())
                {
                    if let Some(route) = routes.find_by_name(name).filter(|r| r.group.is_none()) {
                        report(route);
                    }
                }
            }
        }
    }

    for route in routes.iter().filter(|r| r.is_credentialed()) {
        if route.declared == Visibility::Public {
            panic!(
                "route '{}': declared public but sits behind an auth layer.",
                route.name
            );
        }
        if route.is_permissive() {
            continue;
        }
        match config.auth_scheme {
            AuthScheme::Bearer
                if route.websocket && routes.auth_flow().is_some_and(|f| f.ws_ticket.is_none()) =>
            {
                panic!(
                    "route '{}': [ws][auth] without a ws_ticket mint. Declare \
                     `ws_ticket: register!(...)` in auth_scheme!.",
                    route.name
                );
            }
            _ => {}
        }
    }

    generate_client(routes, config)
}

fn generate_client(routes: &RouteCollection, config: &GeneratorConfig) -> String {
    let mut out = String::new();

    // Header
    w!(out, "// Auto-generated by axotyped. Do not edit.");
    w!(out);

    let lifecycle = routes
        .auth_flow()
        .is_some_and(|f| f.kind != crate::auth_flow::AuthFlowKind::StaticBearer)
        && config.auth_scheme == AuthScheme::Bearer;

    // Collect types used in route signatures.
    let import_prefix = compute_import_prefix(config);
    let custom_types: BTreeSet<&str> = routes
        .iter()
        .flat_map(|r| {
            [
                r.body_type.as_deref(),
                r.response_type.as_deref(),
                r.query_type.as_deref(),
                r.ws_send_type.as_deref(),
                r.ws_receive_type.as_deref(),
            ]
        })
        .flatten()
        .flat_map(extract_type_names)
        .collect();

    for type_name in &custom_types {
        w!(
            out,
            "import type {{ {type_name} }} from \"{import_prefix}/{type_name}\";"
        );
    }
    if !custom_types.is_empty() {
        w!(out);
    }

    w!(out, "export type * from \"{import_prefix}\";");

    let has_flow = routes.auth_flow().is_some();
    let has_typed_ws = routes
        .iter()
        .any(|r| r.websocket && r.ws_send_type.is_some() && r.ws_receive_type.is_some());

    // Runtime imports are aliased (`__Axo*`): user DTOs are imported by
    // their plain names just above, so a response/request type named e.g.
    // `Transport` or `OperationSpec` would otherwise produce two imports
    // with the same local name and break TypeScript checking.
    w!(out, "import {{");
    w!(out, "  ApiError as __AxoApiError,");
    w!(out, "  RUNTIME_SPEC_VERSION as __AxoRuntimeSpecVersion,");
    w!(out, "  createTransport as __AxoCreateTransport,");
    w!(out, "  type OperationSpec as __AxoOperationSpec,");
    w!(out, "  type Transport as __AxoTransport,");
    if has_flow {
        w!(out, "  type AuthStrategy as __AxoAuthStrategy,");
        w!(out, "  type FlowManifest as __AxoFlowManifest,");
    }
    if has_typed_ws {
        w!(out, "  type TypedWebSocket as __AxoTypedWebSocket,");
    }
    w!(
        out,
        "}} from \"{runtime}\";",
        runtime = runtime_import(config)
    );
    w!(out);
    w!(out, "export {{ __AxoApiError as ApiError }};");
    if config.error_class_name != "ApiError" {
        w!(
            out,
            "export {{ __AxoApiError as {err} }};",
            err = config.error_class_name
        );
    }
    // The session layer is the application's now, and these two are what it
    // needs to write one: coalesce parallel renewals, and read the `exp`
    // claim to decide when to renew ahead of a 401. Re-exported so they
    // arrive with the client instead of a second import of the sibling.
    if config.auth_scheme == AuthScheme::Bearer {
        w!(
            out,
            "export {{ jwtExpiry, singleFlight }} from \"{runtime}\";",
            runtime = runtime_import(config)
        );
    }
    if has_typed_ws {
        w!(
            out,
            "export type {{ __AxoTypedWebSocket as TypedWebSocket }};"
        );
    }
    w!(out);
    w!(
        out,
        "if (__AxoRuntimeSpecVersion !== \"{version}\")",
        version = env!("CARGO_PKG_VERSION")
    );
    w!(
        out,
        "  throw new Error(\"client/runtime version drift — regenerate both files together\");"
    );
    w!(out);
    w!(
        out,
        "export const RUNTIME_VERSION = \"{version}\";",
        version = env!("CARGO_PKG_VERSION")
    );
    let op_keys = operations_keys(routes);
    let named_key =
        |name: &str| -> Option<String> { routes.find_by_name(name).map(|r| key_of(&op_keys, r)) };
    let ticket_ref = routes
        .auth_flow()
        .and_then(|f| f.ws_ticket.clone())
        .and_then(|name| named_key(&name));

    w!(out);
    w!(out, "/** Transport policy. */");
    match &ticket_ref {
        Some(key) => w!(
            out,
            "export const TRANSPORT = {{ scheme: \"{scheme}\", wsTicket: \"{key}\" }} as const;",
            scheme = scheme_label(config),
            key = escape_js_string(key)
        ),
        None => w!(
            out,
            "export const TRANSPORT = {{ scheme: \"{scheme}\" }} as const;",
            scheme = scheme_label(config)
        ),
    }
    w!(out);
    write_operations_table(&mut out, routes, &op_keys, ticket_ref.as_deref());
    if let Some(flow) = routes.auth_flow() {
        out.push_str(&auth_strategy_const(flow, routes, &op_keys));
        w!(out);
    }
    out.push_str(&options_interface(config, lifecycle));
    w!(out);
    let factory = &config.factory_name;
    let opts_name = &config.options_interface_name;
    let type_name = derive_type_name(factory);
    w!(
        out,
        "export type {type_name} = ReturnType<typeof {factory}Routes> & {{"
    );
    w!(
        out,
        "  withOptions(override: Partial<{opts_name}>): {type_name};"
    );
    w!(out, "}};");
    w!(out);
    if let Some(helper) = routes
        .auth_flow()
        .filter(|_| lifecycle)
        .and_then(|flow| refresh_helper(flow, routes, &type_name, config.enable_groups))
    {
        out.push_str(&helper);
        w!(out);
    }
    w!(
        out,
        "export function {factory}(options: {opts_name}): {type_name} {{"
    );
    let csrf_field = match config.csrf_header_name.as_deref() {
        Some(name) if config.auth_scheme == AuthScheme::Cookie => {
            format!(", csrfHeader: \"{}\"", escape_js_string(name))
        }
        _ => String::new(),
    };
    w!(out, "  const transport = __AxoCreateTransport(");
    w!(
        out,
        "    {{ ...options, credentials: options.credentials ?? \"{creds}\"{csrf} }},",
        creds = escape_js_string(&config.default_credentials),
        csrf = csrf_field
    );
    w!(out, "    TRANSPORT,");
    w!(out, "    OPERATIONS,");
    w!(out, "  );");
    w!(out);
    w!(
        out,
        "  const withOptions = (override: Partial<{opts_name}>): {type_name} =>"
    );
    w!(out, "    {factory}({{ ...options, ...override }});");
    w!(out);
    w!(out, "  const routes = {factory}Routes(transport);");
    w!(out, "  return Object.assign(routes, {{ withOptions }});");
    w!(out, "}}");
    w!(out);
    w!(
        out,
        "function {factory}Routes(transport: __AxoTransport) {{"
    );
    w!(out, "  return {{");

    if config.enable_groups {
        generate_grouped_routes(&mut out, routes, config, &op_keys);
    } else {
        generate_flat_routes(&mut out, routes, config, &op_keys);
    }

    w!(out, "  }};");
    w!(out, "}}");
    w!(out);

    out
}

/// Generates routes into namespace groups.
fn generate_grouped_routes(
    out: &mut String,
    routes: &RouteCollection,
    config: &GeneratorConfig,
    op_keys: &HashMap<(Option<String>, String), String>,
) {
    let mut ungrouped: Vec<&RouteDefinition> = Vec::new();
    let mut groups: BTreeMap<String, Vec<&RouteDefinition>> = BTreeMap::new();

    for route in routes {
        match &route.group {
            Some(group) => groups.entry(group.clone()).or_default().push(route),
            None => ungrouped.push(route),
        }
    }

    // Lifecycle routes also surface under an `auth` namespace.
    if config.auth_scheme == AuthScheme::Bearer {
        if let Some(flow) = routes.auth_flow() {
            if routes
                .routes()
                .iter()
                .any(|r| r.group.is_none() && r.name == "auth")
            {
                panic!(
                    "route 'auth' collides with the flow's auth namespace. Rename the route with .as_()."
                );
            } else {
                let entry = groups.entry("auth".to_string()).or_default();
                for name in flow
                    .bootstrap
                    .iter()
                    .chain(flow.refresh.iter())
                    .chain(flow.ws_ticket.iter())
                {
                    if let Some(route) = routes.find_by_name(name) {
                        if !entry.iter().any(|r| r.name == route.name) {
                            entry.push(route);
                        }
                    }
                }
            }
        }
    }

    for route in &ungrouped {
        if route.websocket && !has_ws_contract(route) {
            continue;
        }
        write!(out, "    ").unwrap();
        generate_route_method(out, route, 4, config, op_keys);
        w!(out, ",");
    }

    for (group_name, group_routes) in &groups {
        if !ungrouped.is_empty() || groups.keys().next() != Some(group_name) {
            w!(out);
        }
        // Group names are quoted.
        w!(out, "    \"{}\": {{", escape_js_string(group_name));
        for route in group_routes {
            if route.websocket && !has_ws_contract(route) {
                continue;
            }
            write!(out, "      ").unwrap();
            generate_route_method(out, route, 6, config, op_keys);
            w!(out, ",");
        }
        w!(out, "    }},");
    }
}

/// Generates routes without grouping.
///
/// Method names share one top-level object here (unlike the grouped layout,
/// where groups namespace them), so a duplicate name would emit an invalid
/// object literal — or, in plain JS, silently shadow the first endpoint.
fn generate_flat_routes(
    out: &mut String,
    routes: &RouteCollection,
    config: &GeneratorConfig,
    op_keys: &HashMap<(Option<String>, String), String>,
) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for route in routes {
        if route.websocket && !has_ws_contract(route) {
            continue;
        }
        if !seen.insert(route.name.as_str()) {
            panic!(
                "route '{}': duplicate method name with enable_groups = false — the \
                 flat client would emit a duplicate key. Rename with .as_().",
                route.name
            );
        }
        write!(out, "    ").unwrap();
        generate_route_method(out, route, 4, config, op_keys);
        w!(out, ",");
    }
}

/// Generates a single route method bound to an OPERATIONS descriptor.
fn generate_route_method(
    out: &mut String,
    route: &RouteDefinition,
    indent: usize,
    config: &GeneratorConfig,
    op_keys: &HashMap<(Option<String>, String), String>,
) {
    let large_int = config.large_int_type.as_str();
    let key = op_ref(&key_of(op_keys, route));
    let args = call_args(route);

    if route.websocket {
        let params = generate_params(route, large_int);
        let pad = " ".repeat(indent + 2);
        emit_method_name(out, &route.name);
        let send_ts = rust_type_to_ts_with(
            route.ws_send_type.as_deref().unwrap_or("ClientEvent"),
            large_int,
        );
        let recv_ts = rust_type_to_ts_with(
            route.ws_receive_type.as_deref().unwrap_or("ServerEvent"),
            large_int,
        );
        if params.is_empty() {
            write!(
                out,
                ": () => transport.connect<{send_ts}, {recv_ts}>(OPERATIONS{key}{args})"
            )
            .unwrap();
        } else {
            write!(
                out,
                ": ({params}) =>\n{pad}transport.connect<{send_ts}, {recv_ts}>(OPERATIONS{key}{args})"
            )
            .unwrap();
        }
    } else if route.redirect {
        let params = generate_params(route, large_int);
        emit_method_name(out, &route.name);
        if params.is_empty() {
            write!(out, ": () => transport.urlFor(OPERATIONS{key}{args})").unwrap();
        } else {
            write!(
                out,
                ": ({params}) => transport.urlFor(OPERATIONS{key}{args})"
            )
            .unwrap();
        }
    } else {
        let params = generate_params(route, large_int);
        let return_type = route
            .response_type
            .as_ref()
            .map(|t| rust_type_to_ts_with(t, large_int))
            .unwrap_or_else(|| "void".into());
        emit_method_name(out, &route.name);
        if params.is_empty() {
            write!(
                out,
                ": () => transport.execute<{return_type}>(OPERATIONS{key}{args})"
            )
            .unwrap();
        } else {
            let pad = " ".repeat(indent + 2);
            write!(
                out,
                ": ({params}) =>\n{pad}transport.execute<{return_type}>(OPERATIONS{key}{args})"
            )
            .unwrap();
        }
    }
}

/// Derives a type name from a factory name (`createFooClient` -> `FooClient`).
fn derive_type_name(factory_name: &str) -> String {
    factory_name
        .strip_prefix("create")
        .unwrap_or(factory_name)
        .to_string()
}

// ---------------------------------------------------------------------------
// File I/O and check
// ---------------------------------------------------------------------------

/// Run a format command on the given file path.
fn run_format_command(format_command: &str, file_path: &str) -> Result<(), std::io::Error> {
    let parts: Vec<&str> = format_command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "format_command is empty",
        ));
    }

    let output = Command::new(parts[0])
        .args(&parts[1..])
        .arg(file_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(std::io::Error::other(format!(
            "format command '{}' exited with {}: {}",
            format_command,
            output.status,
            stderr.trim()
        )));
    }

    Ok(())
}

/// Generate the client and its runtime module, writing both files.
pub fn generate_to_file(
    routes: &RouteCollection,
    config: &GeneratorConfig,
) -> Result<(), std::io::Error> {
    let content = generate(routes, config);
    let runtime = generate_runtime_source();

    if let Some(parent) = Path::new(&config.output_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&config.output_path, &content)?;
    let runtime_path = runtime_output_path(config);
    std::fs::write(&runtime_path, &runtime)?;

    if let Some(ref cmd) = config.format_command {
        run_format_command(cmd, &config.output_path)?;
        run_format_command(cmd, &runtime_path.to_string_lossy())?;
    }

    Ok(())
}

/// Removes the temp file on drop.
struct TempFile(std::path::PathBuf);

impl TempFile {
    /// Creates a unique temp file.
    fn create(prefix: &str, contents: &[u8]) -> Result<Self, std::io::Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut attempts = 0u32;
        loop {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 | (d.as_secs() << 20))
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "{prefix}_{}_{}_{}",
                std::process::id(),
                nanos,
                unique
            ));

            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true) // fails if anything already exists at the path
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(contents)?;
                    return Ok(TempFile(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    attempts += 1;
                    if attempts > 16 {
                        return Err(std::io::Error::other(
                            "could not allocate a unique temporary file path",
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.0.to_string_lossy()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Returns `Ok(())` if generated output matches the committed files.
pub fn check(routes: &RouteCollection, config: &GeneratorConfig) -> Result<(), CheckError> {
    let generated = generate(routes, config);
    let runtime_generated = generate_runtime_source();

    for (path, content) in [
        (config.output_path.clone(), generated),
        (
            runtime_output_path(config).to_string_lossy().into_owned(),
            runtime_generated,
        ),
    ] {
        let expected = if let Some(ref cmd) = config.format_command {
            let temp = TempFile::create("axotyped_check", content.as_bytes()).map_err(|e| {
                CheckError::ReadError {
                    path: "temporary file".into(),
                    error: e,
                }
            })?;
            let temp_str = temp.as_str().to_string();

            run_format_command(cmd, &temp_str).map_err(|e| CheckError::FormatError {
                command: cmd.clone(),
                error: e,
            })?;

            std::fs::read_to_string(&temp.0).map_err(|e| CheckError::ReadError {
                path: temp_str,
                error: e,
            })?
            // temp file auto-removed on drop
        } else {
            content
        };

        let existing = std::fs::read_to_string(&path).map_err(|e| CheckError::ReadError {
            path: path.clone(),
            error: e,
        })?;

        if expected != existing {
            return Err(CheckError::OutOfSync { path });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_type_to_ts_primitives() {
        assert_eq!(rust_type_to_ts("String"), "string");
        assert_eq!(rust_type_to_ts("bool"), "boolean");
        assert_eq!(rust_type_to_ts("u32"), "number");
        // Wide integers map to bigint by default in the test helper; the
        // configurable mapping is covered separately.
        assert_eq!(rust_type_to_ts("i64"), "bigint");
        assert_eq!(rust_type_to_ts("Uuid"), "string");
        assert_eq!(rust_type_to_ts("f64"), "number");
    }

    #[test]
    fn test_rust_type_to_ts_vec() {
        assert_eq!(rust_type_to_ts("Vec<String>"), "string[]");
        assert_eq!(rust_type_to_ts("Vec<UserResponse>"), "UserResponse[]");
    }

    #[test]
    fn test_rust_type_to_ts_option() {
        assert_eq!(rust_type_to_ts("Option<String>"), "string | null");
        assert_eq!(
            rust_type_to_ts("Option<UserResponse>"),
            "UserResponse | null"
        );
    }

    #[test]
    fn test_rust_type_to_ts_custom() {
        assert_eq!(rust_type_to_ts("RegisterRequest"), "RegisterRequest");
        assert_eq!(rust_type_to_ts("LoginResponse"), "LoginResponse");
    }

    #[test]
    fn test_derive_type_name() {
        assert_eq!(derive_type_name("createYAuthClient"), "YAuthClient");
        assert_eq!(derive_type_name("createApiClient"), "ApiClient");
        assert_eq!(derive_type_name("myClient"), "myClient");
    }

    #[test]
    fn test_is_primitive_type() {
        assert!(is_primitive_type("String"));
        assert!(is_primitive_type("bool"));
        assert!(is_primitive_type("u32"));
        assert!(is_primitive_type("Uuid"));
        assert!(!is_primitive_type("RegisterRequest"));
        assert!(is_primitive_type("Vec<String>"));
        assert!(!is_primitive_type("Vec<UserResponse>"));
    }

    #[test]
    fn test_extract_type_names_primitive() {
        assert!(extract_type_names("String").is_empty());
        assert!(extract_type_names("bool").is_empty());
        assert!(extract_type_names("u32").is_empty());
    }

    #[test]
    fn test_extract_type_names_plain() {
        assert_eq!(
            extract_type_names("RegisterRequest"),
            vec!["RegisterRequest"]
        );
    }

    #[test]
    fn test_extract_type_names_vec() {
        assert_eq!(
            extract_type_names("Vec<UserResponse>"),
            vec!["UserResponse"]
        );
        assert!(extract_type_names("Vec<String>").is_empty());
    }

    #[test]
    fn test_extract_type_names_option() {
        assert_eq!(
            extract_type_names("Option<UserResponse>"),
            vec!["UserResponse"]
        );
    }

    #[test]
    fn test_extract_type_names_generic() {
        let mut names = extract_type_names("ContentResponse<Dialog>");
        names.sort();
        assert_eq!(names, vec!["ContentResponse", "Dialog"]);
    }

    #[test]
    fn test_extract_type_names_nested_generic() {
        let mut names = extract_type_names("Vec<ContentResponse<Dialog>>");
        names.sort();
        assert_eq!(names, vec!["ContentResponse", "Dialog"]);
    }

    #[test]
    fn test_extract_type_names_generic_with_primitive() {
        assert_eq!(extract_type_names("Pagination<String>"), vec!["Pagination"]);
    }

    #[test]
    fn test_extract_type_names_nested_multi_param() {
        // Custom multi-param generic: PaginatedResult<Vec<Item>, Meta>
        let mut names = extract_type_names("PaginatedResult<Vec<Item>, Meta>");
        names.sort();
        assert_eq!(names, vec!["Item", "Meta", "PaginatedResult"]);
    }

    #[test]
    fn test_compute_import_prefix_explicit() {
        let config = GeneratorConfig {
            type_import_prefix: "../types".into(),
            ..Default::default()
        };
        assert_eq!(compute_import_prefix(&config), "../types");
    }
}
