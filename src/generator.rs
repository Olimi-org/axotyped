use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Component, Path};
use std::process::Command;

use crate::types::{HttpMethod, RouteCollection, RouteDefinition};

/// Shorthand for `writeln!(...).unwrap()` — writing to `String` is infallible.
macro_rules! w {
    ($dst:expr) => { writeln!($dst).unwrap() };
    ($dst:expr, $($arg:tt)*) => { writeln!($dst, $($arg)*).unwrap() };
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
    /// Whether to include credentials by default.
    pub default_credentials: String,
    /// Import path prefix for types (relative from generated file to bindings dir).
    /// If empty, computed from bindings_dir relative to output_path.
    pub type_import_prefix: String,
    /// Optional shell command to format the generated file (e.g., `"biome format --write"`).
    /// The output file path is appended as the last argument.
    pub format_command: Option<String>,
    /// Path of the server endpoint issuing single-use WebSocket tickets
    /// (e.g., `Some("/ws/ticket")`).
    ///
    /// When set, every `[ws][auth]` route is generated as an async ticket
    /// handshake: an authenticated POST returns `{ ticket: string }`, and only
    /// that short-lived single-use proof touches the WS URL — keeping
    /// long-lived credentials out of access logs, proxies, and history.
    ///
    /// When `None` (default), `[ws][auth]` routes generate a client with **no**
    /// credential pathway, and [`generate_with_warnings`] emits a diagnostic.
    pub ws_ticket_path: Option<String>,
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
            default_credentials: "include".into(),
            type_import_prefix: String::new(),
            format_command: None,
            ws_ticket_path: None,
        }
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
    "f64", "usize", "isize",
];

/// Rust stdlib container types that don't need TS imports.
/// Only Vec and Option realistically appear as wire types — they're handled
/// by converting to TS equivalents (Vec → T[], Option → T | null).
/// Others are listed for completeness but won't appear in practice.
const CONTAINERS: &[&str] = &["Vec", "Option"];

/// Recursively strip `Vec<>`/`Option<>` wrappers, returning the innermost type name.
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

/// Convert a Rust type name (from `stringify!`) to a TypeScript type string.
fn rust_type_to_ts(rust_type: &str) -> String {
    let t = rust_type.trim();
    if let Some(inner) = t.strip_prefix("Vec<").and_then(|s| s.strip_suffix('>')) {
        return format!("{}[]", rust_type_to_ts(inner));
    }
    if let Some(inner) = t.strip_prefix("Option<").and_then(|s| s.strip_suffix('>')) {
        return format!("{} | null", rust_type_to_ts(inner));
    }
    match t {
        "String" | "&str" | "Uuid" => "string".into(),
        "bool" => "boolean".into(),
        _ if PRIMITIVES.contains(&t) => "number".into(),
        _ => t.to_string(),
    }
}

/// Check if a type is a primitive (doesn't need an import).
#[cfg(test)]
fn is_primitive_type(rust_type: &str) -> bool {
    PRIMITIVES.contains(&unwrap_inner(rust_type))
}

/// Extract all custom type names from a type string for import generation.
///
/// Handles generics, Vec, Option, and nested types:
/// - `ContentResponse<Dialog>` → `["ContentResponse", "Dialog"]`
/// - `Vec<UserResponse>` → `["UserResponse"]`
/// - `Option<ContentResponse<Dialog>>` → `["ContentResponse", "Dialog"]`
/// - `String` → `[]` (primitive)
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

/// Split generic parameters on commas, respecting `<`/`>` nesting.
///
/// `"ContentResponse<Dialog>, ApiError"` → `["ContentResponse<Dialog>", " ApiError"]`
/// `"A<B, C>, D"` → `["A<B, C>", " D"]`
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

/// Compute the import path prefix for type imports.
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
        prefix.push('/');
        prefix.push_str(&suffix);
    }
    prefix
}

/// Build the path template string for TypeScript.
///
/// `/admin/users/{id}` -> `` `/admin/users/${encodeURIComponent(id)}` ``
///
/// Behavior notes:
/// - literal text is escaped for its target string context — backslash,
///   backtick, and `${` are neutralized inside template literals and `\`/`"`
///   inside plain strings — so every character in a route path is emitted as
///   inert string data;
/// - brace groups become `${encodeURIComponent(<param>)}` placeholders whose
///   names go through the exact same deterministic sanitization as
///   [`crate::extract_path_params`], keeping signatures and templates in sync;
/// - parameters are URL-encoded at runtime so values cannot smuggle path
///   traversal (`../`) or query syntax (`?`, `#`) into the request path.
fn build_path_template(path: &str) -> String {
    if !path.contains('{') {
        return format!("\"{}\"", escape_double_quoted(path));
    }

    let mut template = String::new();
    // raw name -> emitted name, mirroring scan order in `extract_path_params`
    let mut seen: Vec<(&str, String)> = Vec::new();
    let mut rest = path;

    loop {
        match rest.find('{') {
            None => {
                push_escaped_template_text(&mut template, rest);
                break;
            }
            Some(open) => {
                push_escaped_template_text(&mut template, &rest[..open]);
                let after = &rest[open..];
                match after[1..].find('}') {
                    None => {
                        // Unterminated brace: emit as inert literal text.
                        push_escaped_template_text(&mut template, after);
                        break;
                    }
                    Some(close_rel) => {
                        let raw = &after[1..1 + close_rel];
                        let safe = match seen.iter().find(|(r, _)| *r == raw) {
                            Some((_, safe)) => safe.clone(),
                            None => {
                                let safe = crate::types::sanitize_param_name(raw, seen.len());
                                seen.push((raw, safe.clone()));
                                safe
                            }
                        };
                        template.push_str("${encodeURIComponent(");
                        template.push_str(&safe);
                        template.push_str(")}");
                        rest = &after[1 + close_rel + 1..];
                    }
                }
            }
        }
    }

    format!("`{template}`")
}

/// Escape `text` for interpolation inside a JS double-quoted string literal.
fn escape_double_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// Escape `text` for interpolation inside a JS template literal: neutralizes
/// backslash, backtick, and `${` sequence starts.
fn push_escaped_template_text(out: &mut String, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' if chars.peek() == Some(&'{') => out.push_str("$\\{"),
            _ => out.push(c),
        }
    }
}

/// Escape `text` for use as a double-quoted JS property key / string:
/// backslash, quote, control characters, and line-separator code points that
/// are valid JSON escapes but syntactically dangerous in JS source.
pub(crate) fn escape_js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
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

/// Generate a function's parameter list for a route.
fn generate_params(route: &RouteDefinition) -> String {
    let mut params = Vec::new();
    for param in &route.path_params {
        params.push(format!("{}: string", param.name));
    }
    if let Some(ref body_type) = route.body_type {
        params.push(format!("body: {}", rust_type_to_ts(body_type)));
    }
    if let Some(ref query_type) = route.query_type {
        params.push(format!("query?: {}", rust_type_to_ts(query_type)));
    }
    params.join(", ")
}

/// Generate the request options object literal for a route.
fn generate_request_options(route: &RouteDefinition) -> String {
    let mut opts = Vec::new();
    if route.method != HttpMethod::Get {
        opts.push(format!("method: \"{}\"", route.method.as_str()));
    }
    if route.auth {
        opts.push("auth: true".into());
    }
    if route.body_type.is_some() {
        opts.push("body".into());
    }
    if route.query_type.is_some() {
        opts.push("query".into());
    }
    if opts.is_empty() {
        String::new()
    } else {
        format!(", {{ {} }}", opts.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Template constants for static TypeScript blocks
// ---------------------------------------------------------------------------

const ERROR_CLASS: &str = r#"export class __ERROR__ extends Error {
  constructor(message: string, public status: number, public body?: unknown) {
    super(message);
    this.name = "__ERROR__";
  }
}
"#;

const OPTIONS_INTERFACE: &str = r#"export interface __OPTS__ {
  baseUrl: string;
  getToken?: () => Promise<string | null>;
  credentials?: RequestCredentials;
  fetch?: typeof fetch;
  onError?: (error: __ERROR__) => void;
  /**
   * Opt-in ONLY for development against a non-loopback http:// target
   * (e.g. an Expo device hitting your LAN IP). Loopback hosts
   * (localhost / 127.0.0.1 / ::1 / *.localhost) are always permitted over
   * http without this flag. Never set in production — CI should assert its
   * absence.
   */
  allowInsecureHttp?: boolean;
}
"#;

const REQUEST_OPTIONS_TYPE: &str = "\
type RequestOptions = {
  method?: string;
  body?: unknown;
  query?: Record<string, unknown>;
  auth?: boolean;
};
";

const REQUEST_HELPER: &str = r#"function assertSecureTransport(
  url: string,
  allowInsecureHttp: boolean,
): void {
  // Transport guard: credentials must only ride https. Loopback hosts are
  // inherently local and always allowed over http; anything else requires
  // the explicit allowInsecureHttp development opt-in. Fails closed on
  // unparseable URLs and non-http(s) protocols.
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error(
      `Client produced an unparseable URL (${JSON.stringify(url)}); refusing to send credentials.`,
    );
  }
  if (parsed.protocol === "https:") return;
  const h = parsed.hostname;
  const isLoopback =
    h === "localhost" ||
    h.endsWith(".localhost") ||
    h === "127.0.0.1" ||
    h === "::1" ||
    h === "[::1]";
  const insecureAllowed =
    parsed.protocol === "http:" && (isLoopback || allowInsecureHttp);
  if (!insecureAllowed) {
    throw new Error(
      parsed.protocol === "http:"
        ? `Refusing to send credentials over http:// to non-loopback host "${h}". Use https:// in production; set allowInsecureHttp on the client options for LAN/device development.`
        : `Unsupported protocol ${parsed.protocol} for credential-bearing requests.`,
    );
  }
}

function createRequest(options: __OPTS__) {
  const { baseUrl, credentials = "__CREDS__" } = options;
  async function request<T>(
    path: string,
    opts: RequestOptions = {},
  ): Promise<T> {
    const { method = "GET", body, query, auth } = opts;
    // Resolve fetch at call time (not at client creation) so OTel
    // instrumentation patches are picked up even when the client
    // module is imported before telemetry initializes.
    const fetchFn = options.fetch ?? globalThis.fetch;

    let url = `${baseUrl}${path}`;
    if (query) {
      const params = new URLSearchParams();
      for (const [key, value] of Object.entries(query)) {
        if (value !== undefined && value !== null) {
          params.set(key, String(value));
        }
      }
      const qs = params.toString();
      if (qs) url += `?${qs}`;
    }

    const headers: Record<string, string> = { "Content-Type": "application/json" };

    // Credentials are only sent over https, to loopback hosts over http,
    // or when the client was explicitly configured with allowInsecureHttp.
    assertSecureTransport(url, options.allowInsecureHttp === true);

    // An [auth] route requires a token: without one configured or returned,
    // the request is aborted rather than sent unauthenticated.
    if (auth) {
      if (!options.getToken) {
        throw new Error(
          `Route declared [auth] but options.getToken was not configured (${method} ${path})`,
        );
      }
      const token = await options.getToken();
      if (!token) {
        throw new Error(
          `options.getToken() returned no token for an [auth] route (${method} ${path})`,
        );
      }
      headers.Authorization = `Bearer ${token}`;
    }

    const response = await fetchFn(url, {
      method,
      credentials,
      headers,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });

    if (!response.ok) {
      const text = await response.text();
      let message: string;
      let errorBody: unknown;
      try {
        const json = JSON.parse(text);
        message = json.error ?? json.message ?? text;
        errorBody = json;
      } catch {
        message = text;
      }
      const error = new __ERROR__(message, response.status, errorBody);
      if (options.onError) options.onError(error);
      throw error;
    }

    const text = await response.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  return request;
}
"#;

const TYPED_WS_INTERFACE: &str = r#"export interface TypedWebSocket<TSend, TReceive> {
  send(event: TSend): void;
  onOpen(handler: () => void): void;
  onMessage(handler: (event: TReceive) => void): void;
  onError(handler: (event: Event) => void): void;
  onClose(handler: (event: CloseEvent) => void): void;
  close(code?: number, reason?: string): void;
  readonly readyState: number;
}
"#;

const TYPED_WS_HELPER: &str = r#"function createTypedWebSocket<TSend, TReceive>(ws: WebSocket): TypedWebSocket<TSend, TReceive> {
  return {
    send(event: TSend) { ws.send(JSON.stringify(event)); },
    onOpen(handler: () => void) { ws.addEventListener("open", handler); },
    onMessage(handler: (event: TReceive) => void) {
      ws.addEventListener("message", (raw: MessageEvent) => {
        handler(JSON.parse(raw.data as string) as TReceive);
      });
    },
    onError(handler: (event: Event) => void) { ws.addEventListener("error", handler); },
    onClose(handler: (event: CloseEvent) => void) { ws.addEventListener("close", handler); },
    close(code?: number, reason?: string) { ws.close(code, reason); },
    get readyState() { return ws.readyState; },
  };
}
"#;

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

/// Generate the full TypeScript client source code.
///
/// Discards diagnostics — prefer [`generate_with_warnings`] in CI/codegen
/// entry points so configuration gaps (e.g. `[ws][auth]` without a ticket
/// endpoint) surface instead of passing silently.
pub fn generate(routes: &RouteCollection, config: &GeneratorConfig) -> String {
    generate_with_warnings(routes, config).0
}

/// Generate the client source alongside non-fatal diagnostics.
///
/// Warnings are human-readable strings describing routes whose generated
/// client cannot honor their declared metadata — currently:
/// - a `[ws][auth]` route while [`GeneratorConfig::ws_ticket_path`] is unset:
///   browsers cannot set headers on the WS handshake, so the generated client
///   has no credential pathway and consumers will resort to query-string
///   tokens.
pub fn generate_with_warnings(
    routes: &RouteCollection,
    config: &GeneratorConfig,
) -> (String, Vec<String>) {
    let mut warnings = Vec::new();
    if config.ws_ticket_path.is_none() {
        for route in routes.iter().filter(|r| r.websocket && r.auth) {
            warnings.push(format!(
                "route '{}': declared [ws][auth] but the generated WebSocket client has no \
                 credential pathway (browsers cannot set headers on the WS handshake). Set \
                 GeneratorConfig::ws_ticket_path to emit the ticket-handshake flow, otherwise \
                 consumers will push tokens into the query string where they leak to logs.",
                route.name
            ));
        }
    }

    (generate_client(routes, config), warnings)
}

fn generate_client(routes: &RouteCollection, config: &GeneratorConfig) -> String {
    let mut out = String::new();

    // Header
    w!(out, "// Auto-generated by axotyped. Do not edit.");
    w!(out);

    // Collect types used in route signatures so we can import them.
    // The bindings directory also has a barrel index.ts (generated by
    // export_types) that re-exports every ts-rs generated type including
    // transitive dependencies.
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

    // Re-export all types from the bindings barrel so consumers can
    // import any generated type (including transitive deps like DialogLine)
    // directly from "@tutor-api".
    w!(out, "export type * from \"{import_prefix}\";");

    // Static blocks via template substitution
    let error_name = &config.error_class_name;
    let opts_name = &config.options_interface_name;
    let default_creds = &config.default_credentials;

    let substitute = |template: &str| -> String {
        template
            .replace("__ERROR__", error_name)
            .replace("__OPTS__", opts_name)
            .replace("__CREDS__", default_creds)
    };

    out.push_str(&substitute(ERROR_CLASS));
    w!(out);
    out.push_str(&substitute(OPTIONS_INTERFACE));
    w!(out);
    out.push_str(REQUEST_OPTIONS_TYPE);
    w!(out);
    out.push_str(&substitute(REQUEST_HELPER));
    w!(out);

    // Typed WebSocket helpers — only if any WS route exists
    let has_ws = routes.iter().any(|r| r.websocket);
    if has_ws {
        out.push_str(TYPED_WS_INTERFACE);
        w!(out);
        out.push_str(TYPED_WS_HELPER);
        w!(out);
    }

    // Factory function
    let factory = &config.factory_name;
    w!(out, "export function {factory}(options: {opts_name}) {{");
    w!(out, "  const request = createRequest(options);");
    w!(out);
    w!(out, "  return {{");

    if config.enable_groups {
        generate_grouped_routes(&mut out, routes, config);
    } else {
        generate_flat_routes(&mut out, routes, config);
    }

    w!(out, "  }};");
    w!(out, "}}");
    w!(out);

    // Type export
    let type_name = derive_type_name(factory);
    w!(
        out,
        "export type {type_name} = ReturnType<typeof {factory}>;"
    );

    out
}

/// Generate routes organized into groups (nested objects).
fn generate_grouped_routes(out: &mut String, routes: &RouteCollection, config: &GeneratorConfig) {
    let mut ungrouped: Vec<&RouteDefinition> = Vec::new();
    let mut groups: BTreeMap<String, Vec<&RouteDefinition>> = BTreeMap::new();

    for route in routes {
        match &route.group {
            Some(group) => groups.entry(group.clone()).or_default().push(route),
            None => ungrouped.push(route),
        }
    }

    for route in &ungrouped {
        write!(out, "    ").unwrap();
        generate_route_method(out, route, 4, config);
        w!(out, ",");
    }

    for (group_name, group_routes) in &groups {
        if !ungrouped.is_empty() || groups.keys().next() != Some(group_name) {
            w!(out);
        }
        // Group names come from free-form &str at registration time; quote and
        // emit it as a quoted, escaped property key.
        w!(out, "    \"{}\": {{", escape_js_string(group_name));
        for route in group_routes {
            write!(out, "      ").unwrap();
            generate_route_method(out, route, 6, config);
            w!(out, ",");
        }
        w!(out, "    }},");
    }
}

/// Generate routes in a flat structure (no grouping).
fn generate_flat_routes(out: &mut String, routes: &RouteCollection, config: &GeneratorConfig) {
    for route in routes {
        write!(out, "    ").unwrap();
        generate_route_method(out, route, 4, config);
        w!(out, ",");
    }
}

/// Generate a single route method.
fn generate_route_method(
    out: &mut String,
    route: &RouteDefinition,
    indent: usize,
    config: &GeneratorConfig,
) {
    if route.websocket {
        generate_ws_method(out, route, indent, config);
    } else if route.redirect {
        generate_redirect_method(out, route, indent);
    } else {
        let name = &route.name;
        let params = generate_params(route);
        let path_template = build_path_template(&route.path);
        let return_type = route
            .response_type
            .as_ref()
            .map(|t| rust_type_to_ts(t))
            .unwrap_or_else(|| "void".into());
        let opts = generate_request_options(route);

        if params.is_empty() {
            write!(
                out,
                "{name}: () => request<{return_type}>({path_template}{opts})"
            )
            .unwrap();
        } else {
            let pad = " ".repeat(indent + 2);
            write!(
                out,
                "{name}: ({params}) =>\n{pad}request<{return_type}>({path_template}{opts})"
            )
            .unwrap();
        }
    }
}

/// Generate a redirect route (URL-builder, not fetch).
fn generate_redirect_method(out: &mut String, route: &RouteDefinition, indent: usize) {
    let name = &route.name;
    let path_template = build_path_template(&route.path);
    let path_inner = &path_template[1..path_template.len() - 1];
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    let pad4 = " ".repeat(indent + 4);

    if let Some(ref query_type) = route.query_type {
        let query_ts = rust_type_to_ts(query_type);
        let mut fn_params = Vec::new();
        for param in &route.path_params {
            fn_params.push(format!("{}: string", param.name));
        }
        fn_params.push(format!("query?: {query_ts}"));
        let all_params = fn_params.join(", ");

        w!(out, "{name}: ({all_params}) => {{");
        w!(out, "{pad2}let url = `${{options.baseUrl}}{path_inner}`;");
        w!(out, "{pad2}if (query) {{");
        w!(out, "{pad4}const params = new URLSearchParams();");
        w!(
            out,
            "{pad4}for (const [key, value] of Object.entries(query)) {{"
        );
        w!(
            out,
            "{pad4}  if (value !== undefined && value !== null) params.set(key, String(value));"
        );
        w!(out, "{pad4}}}");
        w!(out, "{pad4}const qs = params.toString();");
        w!(out, "{pad4}if (qs) url += `?${{qs}}`;");
        w!(out, "{pad2}}}");
        write!(out, "{pad2}return url;\n{pad}}}").unwrap();
    } else {
        let params = generate_params(route);
        if params.is_empty() {
            write!(out, "{name}: () => `${{options.baseUrl}}{path_inner}`").unwrap();
        } else {
            write!(
                out,
                "{name}: ({params}) => `${{options.baseUrl}}{path_inner}`"
            )
            .unwrap();
        }
    }
}

/// Derive a type name from a factory function name.
///
/// `createYAuthClient` -> `YAuthClient`
fn derive_type_name(factory_name: &str) -> String {
    factory_name
        .strip_prefix("create")
        .unwrap_or(factory_name)
        .to_string()
}

/// Generate a WebSocket route method.
///
/// When send/receive types are defined, produces a TypeScript factory that
/// returns a `TypedWebSocket<S, R>` with typed `send()` and `onMessage()`.
/// Otherwise produces a bare `new WebSocket(url)`.
///
/// Authenticated routes ([`GeneratorConfig::ws_ticket_path`] set + `auth`) are
/// generated as an async ticket handshake: an authenticated POST to the
/// ticket endpoint yields a single-use short-TTL ticket, and only that proof
/// is appended to the WS URL — long-lived credentials never enter the URL,
/// where they would leak to access logs, proxies, and browser history. The
/// server must consume tickets atomically on upgrade.
fn generate_ws_method(
    out: &mut String,
    route: &RouteDefinition,
    indent: usize,
    config: &GeneratorConfig,
) {
    let name = &route.name;
    let path_template = build_path_template(&route.path);
    let path_inner = &path_template[1..path_template.len() - 1];
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    let pad4 = " ".repeat(indent + 4);

    // Ticket-handshake mode applies to [auth] routes when configured:
    let ticket_mode = if route.auth {
        config.ws_ticket_path.as_ref()
    } else {
        None
    };

    // Build function parameters: path params + optional query
    let mut fn_params = Vec::new();
    for param in &route.path_params {
        fn_params.push(format!("{}: string", param.name));
    }
    if let Some(ref query_type) = route.query_type {
        fn_params.push(format!("query?: {}", rust_type_to_ts(query_type)));
    }
    let all_params = fn_params.join(", ");

    // Determine return type
    let has_types = route.ws_send_type.is_some() && route.ws_receive_type.is_some();
    let ws_type = if has_types {
        let send_ts = rust_type_to_ts(route.ws_send_type.as_ref().unwrap());
        let recv_ts = rust_type_to_ts(route.ws_receive_type.as_ref().unwrap());
        format!("TypedWebSocket<{send_ts}, {recv_ts}>")
    } else {
        "WebSocket".into()
    };
    let return_type = if ticket_mode.is_some() {
        format!("Promise<{ws_type}>")
    } else {
        ws_type.clone()
    };

    w!(
        out,
        "{name}: {}({all_params}): {return_type} => {{",
        if ticket_mode.is_some() { "async " } else { "" }
    );

    // Convert http(s) to ws(s)
    w!(
        out,
        "{pad2}const baseUrl = options.baseUrl.replace(/^http/, (m) => m === \"https\" ? \"wss\" : \"ws\");"
    );

    // Ticket handshake first: the long-lived credential rides the header
    // channel; only the single-use proof it returns touches the URL below.
    if let Some(ticket_path) = ticket_mode {
        w!(out);
        w!(
            out,
            "{pad2}// Authenticated ticket roundtrip — server must issue a single-use,"
        );
        w!(
            out,
            "{pad2}// short-TTL ticket bound to the caller and consume it on upgrade."
        );
        w!(
            out,
            "{pad2}const {{ ticket: __wsTicket }} = await request<{{ ticket: string }}>(\"{}\",",
            escape_double_quoted(ticket_path)
        );
        w!(out, "{pad2}  {{ method: \"POST\", auth: true }},");
        w!(out, "{pad2});");
    }

    // Build path with params
    w!(out, "{pad2}let url = `${{baseUrl}}{path_inner}`;");

    // Append query params if present
    if route.query_type.is_some() {
        w!(out, "{pad2}if (query) {{");
        w!(out, "{pad4}const params = new URLSearchParams();");
        w!(
            out,
            "{pad4}for (const [key, value] of Object.entries(query)) {{"
        );
        w!(
            out,
            "{pad4}  if (value !== undefined && value !== null) params.set(key, String(value));"
        );
        w!(out, "{pad4}}}");
        w!(out, "{pad4}const qs = params.toString();");
        w!(out, "{pad4}if (qs) url += `?${{qs}}`;");
        w!(out, "{pad2}}}");
    }

    // Append the single-use ticket last so it composes with any query string
    if ticket_mode.is_some() {
        w!(
            out,
            "{pad2}url += `${{url.includes(\"?\") ? \"&\" : \"?\"}}ticket=${{encodeURIComponent(__wsTicket)}}`;"
        );
    }

    if has_types {
        let send_ts = rust_type_to_ts(route.ws_send_type.as_ref().unwrap());
        let recv_ts = rust_type_to_ts(route.ws_receive_type.as_ref().unwrap());
        w!(
            out,
            "{pad2}assertSecureTransport(url, options.allowInsecureHttp === true);"
        );
        w!(out, "{pad2}const ws = new WebSocket(url);");
        w!(
            out,
            "{pad2}return createTypedWebSocket<{send_ts}, {recv_ts}>(ws);"
        );
    } else {
        w!(
            out,
            "{pad2}assertSecureTransport(url, options.allowInsecureHttp === true);"
        );
        w!(out, "{pad2}return new WebSocket(url);");
    }

    write!(out, "{pad}}}").unwrap();
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

/// Generate the client and write it to a file.
///
/// If `config.format_command` is set, the format command is run on the output file after writing.
pub fn generate_to_file(
    routes: &RouteCollection,
    config: &GeneratorConfig,
) -> Result<(), std::io::Error> {
    let content = generate(routes, config);

    if let Some(parent) = Path::new(&config.output_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&config.output_path, &content)?;

    if let Some(ref cmd) = config.format_command {
        run_format_command(cmd, &config.output_path)?;
    }

    Ok(())
}

/// RAII guard that removes a temp file on drop.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Check if the generated output matches the committed file.
///
/// When `config.format_command` is set, the generated output is written to a temporary file
/// and formatted before comparing, so the check accounts for formatter changes.
///
/// Returns `Ok(())` if in sync, `Err(CheckError)` if not.
pub fn check(routes: &RouteCollection, config: &GeneratorConfig) -> Result<(), CheckError> {
    let generated = generate(routes, config);

    let expected = if let Some(ref cmd) = config.format_command {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp = TempFile(
            std::env::temp_dir().join(format!("axotyped_check_{}_{nanos}.ts", std::process::id())),
        );
        let temp_str = temp.0.to_string_lossy().to_string();

        std::fs::write(&temp.0, &generated).map_err(|e| CheckError::ReadError {
            path: temp_str.clone(),
            error: e,
        })?;

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
        generated
    };

    let existing =
        std::fs::read_to_string(&config.output_path).map_err(|e| CheckError::ReadError {
            path: config.output_path.clone(),
            error: e,
        })?;

    if expected != existing {
        Err(CheckError::OutOfSync {
            path: config.output_path.clone(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_type_to_ts_primitives() {
        assert_eq!(rust_type_to_ts("String"), "string");
        assert_eq!(rust_type_to_ts("bool"), "boolean");
        assert_eq!(rust_type_to_ts("u32"), "number");
        assert_eq!(rust_type_to_ts("i64"), "number");
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
    fn test_build_path_template_no_params() {
        assert_eq!(build_path_template("/register"), "\"/register\"");
        assert_eq!(build_path_template("/admin/users"), "\"/admin/users\"");
    }

    #[test]
    fn test_build_path_template_with_params() {
        assert_eq!(
            build_path_template("/admin/users/{id}"),
            "`/admin/users/${encodeURIComponent(id)}`"
        );
        assert_eq!(
            build_path_template("/orgs/{org}/users/{id}"),
            "`/orgs/${encodeURIComponent(org)}/users/${encodeURIComponent(id)}`"
        );
    }

    #[test]
    fn test_build_path_template_escapes_special_characters() {
        // Backticks / ${ cannot break out of the emitted template literal:
        assert_eq!(
            build_path_template("/x/{id}`+alert(1)+`"),
            "`/x/${encodeURIComponent(id)}\\`+alert(1)+\\``"
        );
        assert!(
            !build_path_template("/x${evil}y").contains("${evil"),
            "dollar-brace sequences in literal text must be escaped"
        );
        // Double-quoted branch escapes quotes/backslashes:
        assert_eq!(build_path_template("/a\"b"), "\"/a\\\"b\"");
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
