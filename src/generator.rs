use std::collections::{BTreeMap, BTreeSet};
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
    /// Ticket endpoint for `[ws][auth]` routes under Bearer (e.g. `Some("/ws/ticket")`).
    /// Emits an async ticket handshake; when `None`, those routes have no
    /// credential pathway and produce a diagnostic.
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
            default_credentials: "same-origin".into(),
            type_import_prefix: String::new(),
            format_command: None,
            ws_ticket_path: None,
            large_int_type: "number".into(),
            auth_scheme: AuthScheme::Bearer,
            csrf_header_name: None,
        }
    }
}

impl GeneratorConfig {
    /// Returns a ts-rs [`Config`](crate::ts::Config) with matching `large_int_type`,
    /// for use with [`RouteCollection::export_types_with`](crate::RouteCollection::export_types_with).
    #[cfg(feature = "ts-rs")]
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
        prefix.push('/');
        prefix.push_str(&suffix);
    }
    prefix
}

/// Builds a JS path expression. Literals are escaped; `{param}` becomes
/// `${encodeURIComponent(param)}` with the same sanitization as `extract_path_params`.
/// e.g. `/admin/users/{id}` -> `` `/admin/users/${encodeURIComponent(id)}` ``.
fn build_path_template(path: &str) -> String {
    if !path.contains('{') {
        return format!("\"{}\"", escape_js_string(path));
    }

    let mut template = String::new();
    // raw name -> emitted name, mirroring the scan order and allocation rules
    // of `extract_path_params`
    let mut seen: Vec<(&str, String)> = Vec::new();
    // Names claimed by valid raw identifiers; synthetics must avoid these.
    let taken: std::collections::BTreeSet<String> = crate::types::scan_taken_param_names(path);
    let mut next_synthetic = 0usize;
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
                                let safe = crate::types::sanitize_param_name_with(
                                    &taken,
                                    &mut next_synthetic,
                                    raw,
                                    seen.len(),
                                );
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

/// Escapes text for a JS template literal (backslash, backtick, `$`).
/// Every `$` is escaped: a following `{` may arrive in a later slice when
/// `build_path_template` splits at `{`, so a peek within this slice is not enough.
fn push_escaped_template_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            // Escape every `$`; a following `{` may arrive in a later slice.
            '$' => out.push_str("\\$"),
            c => out.push(c),
        }
    }
}

/// Escapes text for a double-quoted JS string (control chars, separators,
/// backticks). `$` is escaped too even though `"\\$"` equals `"$"`: callers
/// strip the quotes and re-embed the text in template literals, where a raw
/// `$` before `{` would become a live substitution.
pub(crate) fn escape_js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '`' => out.push_str("\\`"),
            // Re-embed safety: this text may end up inside a template
            // literal, so `$` must never survive unescaped.
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

/// Emits a route name as an object key; quotes and escapes non-identifiers
/// (e.g. names from `.as_(...)`).
fn emit_method_name(out: &mut String, name: &str) {
    if crate::types::is_valid_js_identifier(name) {
        out.push_str(name);
    } else {
        out.push('"');
        out.push_str(&escape_js_string(name));
        out.push('"');
    }
}

/// Generates the parameter list for a route from its path params, body,
/// and query types. Redirect behavior is a server-side route property, so
/// callers take no options.
fn generate_params(route: &RouteDefinition, large_int: &str) -> String {
    let mut params = Vec::new();
    for param in &route.path_params {
        params.push(format!("{}: string", param.name));
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

/// Generates the request options literal for a route. Fully
/// generator-controlled: `method` is always emitted so no caller can change
/// the verb, `permissive` is always pinned so a caller cannot soften a
/// `Private` route into best-effort, and `allowRedirects` reflects the
/// server-side `[allow_redirects]` route property.
fn generate_request_options(route: &RouteDefinition) -> String {
    let mut opts: Vec<String> = Vec::new();
    opts.push(format!("method: \"{}\"", route.method.as_str()));
    match route.visibility {
        Visibility::Private => {
            opts.push("auth: true".into());
            opts.push("permissive: false".into());
        }
        Visibility::Permissive => {
            opts.push("auth: true".into());
            opts.push("permissive: true".into());
        }
        Visibility::Public => {
            opts.push("permissive: false".into());
        }
    }
    opts.push(format!("allowRedirects: {}", route.allow_redirects));
    if route.body_type.is_some() {
        opts.push("body".into());
    }
    if route.query_type.is_some() {
        opts.push("query".into());
    }
    format!(", {{ {} }}", opts.join(", "))
}

// ---------------------------------------------------------------------------
// Template constants for static TypeScript blocks
// ---------------------------------------------------------------------------

const ERROR_CLASS: &str = r#"export class __ERROR__ extends Error {
  constructor(message: string, public status: number, public body?: unknown) {
    super(message);

    // Keep native Error behavior when transpiled to older targets:
    // name non-enumerable, prototype chain intact, stack trace captured.
    Object.defineProperty(this, "name", {
      value: "__ERROR__",
      enumerable: false,
      configurable: true,
    });
    if (Object.setPrototypeOf !== undefined) {
      Object.setPrototypeOf(this, __ERROR__.prototype);
    }
    if ((Error as any).captureStackTrace !== undefined) {
      (Error as any).captureStackTrace(this, this.constructor);
    }
  }
}
"#;

const INSECURE_HTTP_DOC: &str = r#"  /**
   * Opt-in ONLY for development against a non-loopback http:// target
   * (e.g. an Expo device hitting your LAN IP). Loopback hosts
   * (localhost / 127.0.0.1 / ::1 / *.localhost) are always permitted over
   * http without this flag. Never set in production — CI should assert its
   * absence.
   */
  allowInsecureHttp?: boolean;
"#;

/// ClientOptions fields shared by every scheme (everything after the
/// scheme-specific auth fields).
const OPTIONS_TAIL: &str = r#"  credentials?: RequestCredentials;
  fetch?: typeof fetch;
  onError?: (error: __ERROR__) => void;
  /**
   * Default RequestInit merged into every request. Useful for AbortSignals
   * (cancellation/timeouts), cache policy, keepalive, and priority hints.
   * Per-request values passed through a route's options take precedence.
   */
  requestInit?: Omit<RequestInit, "headers" | "method" | "body"> & {
    headers?: Record<string, string>;
  };
"#;

const OPTIONS_INTERFACE_BEARER: &str = r#"export interface __OPTS__ {
  baseUrl: string;
  getToken?: () => Promise<string | null>;
"#;

const OPTIONS_INTERFACE_PLAIN_HEAD: &str = r#"export interface __OPTS__ {
  baseUrl: string;
"#;

const OPTIONS_CSRF_FIELD: &str = r#"  /**
   * Returns the anti-CSRF proof attached as the "__CSRF_NAME__" header on
   * mutating requests (POST / PUT / PATCH / DELETE). Source it from your
   * framework's cookie or meta-tag convention. When unset, mutating requests
   * carry no CSRF header — rely on SameSite cookie attributes server-side.
   */
  csrfToken?: () => Promise<string | null> | string | null;
"#;

const OPTIONS_INTERFACE_NONE: &str = r#"export interface __OPTS__ {
  baseUrl: string;
"#;

/// Assemble the options interface for the configured auth scheme.
fn options_interface(config: &GeneratorConfig) -> String {
    let mut s = String::new();
    match config.auth_scheme {
        AuthScheme::Bearer => s.push_str(OPTIONS_INTERFACE_BEARER),
        AuthScheme::Cookie => {
            s.push_str(OPTIONS_INTERFACE_PLAIN_HEAD);
            if config.csrf_header_name.is_some() {
                s.push_str(OPTIONS_CSRF_FIELD);
            }
        }
        AuthScheme::None => s.push_str(OPTIONS_INTERFACE_NONE),
    }
    s.push_str(OPTIONS_TAIL);
    s.push_str(INSECURE_HTTP_DOC);
    s.push_str("}\n");
    s
}

const REQUEST_OPTIONS_TYPE: &str = "\
/** Route call options. Fully generator-pinned per route — route methods take
    no caller options, so these fields are never user-supplied. */
type RequestOptions = {
  method?: string;
  body?: unknown;
  query?: Record<string, unknown>;
  auth?: boolean;
  /** Attach credentials when available, never require them. */
  permissive?: boolean;
  /** Follow redirects only for credentialless calls to declaring routes. */
  allowRedirects?: boolean;
};
\
/** The `request` helper produced by `createRequest`, for the routes factory. */
type RequestFn = <T>(path: string, opts?: RequestOptions) => Promise<T>;
";

const REQUEST_HELPER_PRE: &str = r#"function assertSecureTransport(
  url: string,
  opts: { allowInsecureHttp: boolean; auth: boolean; credentials: string },
): void {
  const { allowInsecureHttp, auth, credentials } = opts;
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
  if (parsed.protocol === "https:" || parsed.protocol === "wss:") return;
  const h = parsed.hostname;
  const isLoopback =
    h === "localhost" ||
    h.endsWith(".localhost") ||
    h === "127.0.0.1" ||
    h === "::1" ||
    h === "[::1]";
  const isInsecureScheme = parsed.protocol === "http:" || parsed.protocol === "ws:";
  const insecureAllowed =
    isInsecureScheme && (isLoopback || allowInsecureHttp);
  if (!insecureAllowed) {
    throw new Error(
      parsed.protocol === "http:"
        ? `Refusing to send credentials over http:// to non-loopback host "${h}". Use https:// in production; set allowInsecureHttp on the client options for LAN/device development.`
        : `Unsupported protocol ${parsed.protocol} for credential-bearing requests.`,
    );
  }
  // allowInsecureHttp permits the HTTP connection itself, but never for
  // credential-bearing requests to non-loopback hosts. Cookies count:
  // even with auth:false, `credentials: "include"` (or `"same-origin"`
  // to a same-origin http target) still sends session cookies.
  if (isInsecureScheme && !isLoopback && (auth || credentials !== "omit")) {
    throw new Error(
      `Refusing to send credentials over http:// to non-loopback host "${h}". Set allowInsecureHttp for the connection, but credentialed requests (auth: true or credentials !== "omit") still require https:// or a loopback host.`,
    );
  }
}

function createRequest(options: __OPTS__) {
  const { baseUrl, credentials = "__CREDS__" } = options;
  // Bind fetch to its original receiver: calling an unbound
  // globalThis.fetch reference throws "Illegal invocation" in several
  // browser engines.
  const boundFetch =
    options.fetch !== undefined ? options.fetch : globalThis.fetch.bind(globalThis);

  async function request<T>(path: string, opts?: RequestOptions): Promise<T>;
  async function request<T>(
    path: string,
    opts: RequestOptions | undefined,
    rawResponse: true,
  ): Promise<Response>;
  async function request<T>(
    path: string,
    opts: RequestOptions = {},
    rawResponse?: boolean,
  ): Promise<T | Response> {
    const { method = "GET", body, query, auth = false, permissive = false } = opts;
    let url = `${baseUrl}${path}`;
    if (query) {
      const params = new URLSearchParams();
      for (const [key, value] of Object.entries(query)) {
        // Arrays expand to repeated keys (?tag=a&tag=b), the conventional
        // encoding for list-valued parameters.
        for (const v of Array.isArray(value) ? value : [value]) {
          if (v !== undefined && v !== null) {
            params.append(key, String(v));
          }
        }
      }
      const qs = params.toString();
      if (qs) url += `?${qs}`;
    }

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...(options.requestInit?.headers ?? {}),
    };

    // Credentials are only sent over https, to loopback hosts over http,
    // or when the client was explicitly configured with allowInsecureHttp.
    // allowInsecureHttp permits the HTTP connection itself, but never for
    // credential-bearing requests (auth or cookies) to non-loopback hosts.
    assertSecureTransport(url, { allowInsecureHttp: options.allowInsecureHttp === true, auth, credentials });
"#;

/// Auth section of the request helper for [`AuthScheme::Bearer`]: a `Private`
/// route aborts unless a token source is configured **and** resolves — an
/// unauthenticated request is never sent. A `Permissive` route attaches the
/// token when one resolves and otherwise sends anonymously.
const REQUEST_AUTH_BEARER: &str = r#"
    // A [permissive] route attaches credentials when available but never
    // fails for want of them; any other [auth] route requires a token.
    if (auth) {
      if (permissive) {
        if (options.getToken) {
          const token = await options.getToken();
          if (token) headers.Authorization = `Bearer ${token}`;
        }
      } else {
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
    }
"#;

/// Auth section of the request helper for [`AuthScheme::Cookie`]: browsers
/// attach session cookies automatically, so the only way an authenticated
/// route can go out unauthenticated is a consumer explicitly stripping them.
/// `Permissive` routes skip the refusal — anonymous (`omit`) is a valid
/// outcome for them.
const REQUEST_AUTH_COOKIE: &str = r#"
    // An [auth] route rides the browser's session cookies; refuse consumer
    // configurations that would strip them instead of sending the request
    // unauthenticated.
    if (auth && !permissive && (options.credentials ?? "__CREDS__") === "omit") {
      throw new Error(
        `Route declared [auth] but options.credentials is "omit"; refusing to send an unauthenticated request (${method} ${path})`,
      );
    }
"#;

/// Anti-CSRF plumbing, appended under [`AuthScheme::Cookie`] when
/// [`GeneratorConfig::csrf_header_name`] is set.
const REQUEST_CSRF_BLOCK: &str = r#"
    // Attach anti-CSRF proof on mutating requests when a token source is
    // configured. GET/HEAD/OPTIONS are safe methods and never carry it.
    if (!["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase())) {
      const csrfToken = await options.csrfToken?.();
      if (csrfToken) headers["__CSRF_NAME__"] = csrfToken;
    }
"#;

const REQUEST_HELPER_POST: &str = r#"
    // Only credentialless calls to routes declaring `[allow_redirects]`
    // follow redirects; anything carrying auth or cookies refuses, since the
    // guard sees only the initial URL and a 3xx could bounce to http://.
    const canFollowRedirects =
      !auth && credentials === "omit" && opts.allowRedirects === true;
    const response = await boundFetch(url, {
      ...options.requestInit,
      ...(auth ? { cache: "no-store" as const } : {}),
      redirect: canFollowRedirects ? "follow" : "error",
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

    if (rawResponse) return response;

    const text = await response.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  return request;
}
"#;

/// Assemble the request helper for the configured auth scheme.
fn request_helper(config: &GeneratorConfig) -> String {
    let mut s = String::from(REQUEST_HELPER_PRE);
    match config.auth_scheme {
        AuthScheme::Bearer => s.push_str(REQUEST_AUTH_BEARER),
        AuthScheme::Cookie => {
            s.push_str(REQUEST_AUTH_COOKIE);
            if config.csrf_header_name.is_some() {
                s.push_str(REQUEST_CSRF_BLOCK);
            }
        }
        AuthScheme::None => {}
    }
    s.push_str(REQUEST_HELPER_POST);
    s
}

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

/// Generates client source, discarding diagnostics.
/// Use [`generate_with_warnings`] to surface configuration gaps.
pub fn generate(routes: &RouteCollection, config: &GeneratorConfig) -> String {
    generate_with_warnings(routes, config).0
}

/// Generates client source plus diagnostics for routes whose metadata
/// the client cannot honor: public-declared behind an auth layer,
/// `Private` routes with `AuthScheme::None`, or `Private` `[ws][auth]` Bearer
/// routes without `ws_ticket_path`. `Permissive` routes degrade gracefully
/// (anonymous is a valid outcome) and never warn. No diagnostic for Cookie
/// WS routes.
pub fn generate_with_warnings(
    routes: &RouteCollection,
    config: &GeneratorConfig,
) -> (String, Vec<String>) {
    let mut warnings = Vec::new();
    for route in routes.iter().filter(|r| r.is_credentialed()) {
        if route.declared == Visibility::Public {
            warnings.push(format!(
                "route '{}': declared public but sits behind an auth layer; the generated \
                 client requires credentials for it. If the middleware is not user \
                 authentication, register it with `.layer()` instead of `.auth_layer()`.",
                route.name
            ));
            continue;
        }
        // Permissive routes work anonymously, so nothing to report — except
        // the WS credential pathway below, which they share with public.
        if route.is_permissive() {
            continue;
        }
        match config.auth_scheme {
            AuthScheme::None => warnings.push(format!(
                "route '{}': declared authenticated but the client was generated with \
                 auth_scheme \"none\"; emitted requests send no credentials.",
                route.name
            )),
            AuthScheme::Bearer if route.websocket && config.ws_ticket_path.is_none() => {
                warnings.push(format!(
                    "route '{}': declared [ws][auth] but the generated WebSocket client has no \
                     credential pathway (browsers cannot set headers on the WS handshake). Set \
                     GeneratorConfig::ws_ticket_path to emit the ticket-handshake flow, otherwise \
                     consumers will push tokens into the query string where they leak to logs.",
                    route.name
                ));
            }
            _ => {}
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
    // import any generated type (including transitive deps) directly from
    // the generated package. Requires TypeScript 5.0+ (`export type *`).
    w!(out, "export type * from \"{import_prefix}\";");

    // Static blocks via template substitution
    let error_name = &config.error_class_name;
    let opts_name = &config.options_interface_name;
    let default_creds = &config.default_credentials;
    let csrf_name = config.csrf_header_name.as_deref().unwrap_or("X-CSRF-Token");

    let substitute = |template: &str| -> String {
        template
            .replace("__ERROR__", error_name)
            .replace("__OPTS__", opts_name)
            .replace("__CREDS__", &escape_js_string(default_creds))
            .replace("__CSRF_NAME__", &escape_js_string(csrf_name))
    };

    out.push_str(&substitute(ERROR_CLASS));
    w!(out);
    out.push_str(&substitute(&options_interface(config)));
    w!(out);
    out.push_str(REQUEST_OPTIONS_TYPE);
    w!(out);
    out.push_str(&substitute(&request_helper(config)));
    w!(out);

    // Typed WebSocket helpers — only if any WS route exists
    let has_ws = routes.iter().any(|r| r.websocket);
    if has_ws {
        out.push_str(TYPED_WS_INTERFACE);
        w!(out);
        out.push_str(TYPED_WS_HELPER);
        w!(out);
    }

    // Runtime version + factory function
    let factory = &config.factory_name;
    let type_name = derive_type_name(factory);
    w!(
        out,
        "// Runtime semantics version — bump when generated helper behavior changes,"
    );
    w!(out, "// so consumers can detect stale committed artifacts.");
    w!(
        out,
        "export const RUNTIME_VERSION = \"{version}\";",
        version = env!("CARGO_PKG_VERSION")
    );
    w!(out);
    // `{type_name}` resolves acyclically because the routes factory is a
    // separate function; `withOptions`'s self-reference sits under an object
    // property, which TS resolves lazily. The exported factory is emitted
    // before the routes factory (function declarations hoist), keeping the
    // route surface discoverable after `export function` in the file.
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
    w!(
        out,
        "export function {factory}(options: {opts_name}): {type_name} {{"
    );
    w!(out, "  const request = createRequest(options);");
    w!(out);
    w!(
        out,
        "  /** Derived client with the given options merged over this client's. */"
    );
    w!(
        out,
        "  const withOptions = (override: Partial<{opts_name}>): {type_name} =>"
    );
    w!(out, "    {factory}({{ ...options, ...override }});");
    w!(out);
    w!(out, "  const routes = {factory}Routes(request, options);");
    w!(out, "  return Object.assign(routes, {{ withOptions }});");
    w!(out, "}}");
    w!(out);
    w!(
        out,
        "function {factory}Routes(request: RequestFn, options: {opts_name}) {{"
    );
    w!(out, "  return {{");

    if config.enable_groups {
        generate_grouped_routes(&mut out, routes, config);
    } else {
        generate_flat_routes(&mut out, routes, config);
    }

    w!(out, "  }};");
    w!(out, "}}");
    w!(out);

    out
}

/// Generates routes into namespace groups.
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

/// Generates routes without grouping.
fn generate_flat_routes(out: &mut String, routes: &RouteCollection, config: &GeneratorConfig) {
    for route in routes {
        write!(out, "    ").unwrap();
        generate_route_method(out, route, 4, config);
        w!(out, ",");
    }
}

/// Generates a single route method.
fn generate_route_method(
    out: &mut String,
    route: &RouteDefinition,
    indent: usize,
    config: &GeneratorConfig,
) {
    let large_int = config.large_int_type.as_str();
    if route.websocket {
        generate_ws_method(out, route, indent, config);
    } else if route.redirect {
        generate_redirect_method(out, route, indent, large_int);
    } else {
        let name = &route.name;
        let params = generate_params(route, large_int);
        let path_template = build_path_template(&route.path);
        let return_type = route
            .response_type
            .as_ref()
            .map(|t| rust_type_to_ts_with(t, large_int))
            .unwrap_or_else(|| "void".into());
        let opts = generate_request_options(route);

        if params.is_empty() {
            emit_method_name(out, name);
            write!(out, ": () => request<{return_type}>({path_template}{opts})").unwrap();
        } else {
            let pad = " ".repeat(indent + 2);
            emit_method_name(out, name);
            write!(
                out,
                ": ({params}) =>\n{pad}request<{return_type}>({path_template}{opts})"
            )
            .unwrap();
        }
    }
}

/// Generates a redirect route (URL builder, not fetch).
fn generate_redirect_method(
    out: &mut String,
    route: &RouteDefinition,
    indent: usize,
    large_int: &str,
) {
    let name = &route.name;
    let path_template = build_path_template(&route.path);
    let path_inner = &path_template[1..path_template.len() - 1];
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    let pad4 = " ".repeat(indent + 4);


    if let Some(ref query_type) = route.query_type {
        let query_ts = rust_type_to_ts_with(query_type, large_int);
        let mut fn_params = Vec::new();
        for param in &route.path_params {
            fn_params.push(format!("{}: string", param.name));
        }
        fn_params.push(format!("query?: {query_ts}"));
        let all_params = fn_params.join(", ");

        emit_method_name(out, name);
        w!(out, ": ({all_params}) => {{");
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
        let params = generate_params(route, large_int);
        if params.is_empty() {
            emit_method_name(out, name);
            write!(out, ": () => `${{options.baseUrl}}{path_inner}`").unwrap();
        } else {
            emit_method_name(out, name);
            write!(out, ": ({params}) => `${{options.baseUrl}}{path_inner}`").unwrap();
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

/// Generates a WS factory returning `TypedWebSocket<S, R>` (or `WebSocket`).
/// Bearer `[auth]` routes with `ws_ticket_path` use an async ticket handshake.
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

    // Ticket-handshake mode applies to `Private` routes under the Bearer
    // scheme when configured. `Permissive` routes skip it (anonymous must
    // work). Under Cookie, browsers attach session cookies to same-origin
    // WS upgrades natively — no handshake needed.
    let ticket_mode =
        if route.visibility == Visibility::Private && config.auth_scheme == AuthScheme::Bearer {
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
        fn_params.push(format!(
            "query?: {}",
            rust_type_to_ts_with(query_type, config.large_int_type.as_str())
        ));
    }
    let all_params = fn_params.join(", ");

    // Determine return type
    let has_types = route.ws_send_type.is_some() && route.ws_receive_type.is_some();
    let ws_type = if has_types {
        let large_int = config.large_int_type.as_str();
        let send_ts = rust_type_to_ts_with(route.ws_send_type.as_ref().unwrap(), large_int);
        let recv_ts = rust_type_to_ts_with(route.ws_receive_type.as_ref().unwrap(), large_int);
        format!("TypedWebSocket<{send_ts}, {recv_ts}>")
    } else {
        "WebSocket".into()
    };
    let return_type = if ticket_mode.is_some() {
        format!("Promise<{ws_type}>")
    } else {
        ws_type.clone()
    };

    emit_method_name(out, name);
    w!(
        out,
        ": {}({all_params}): {return_type} => {{",
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
            escape_js_string(ticket_path)
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

    // Cookie `Private` routes ride same-origin session cookies natively — the
    // WebSocket API has no credentials option, so `"omit"` would silently
    // send the upgrade unauthenticated. Refuse like the fetch path does.
    // `Permissive` routes allow anonymous upgrades.
    if config.auth_scheme == AuthScheme::Cookie && route.visibility == Visibility::Private {
        w!(
            out,
            "{pad2}if ((options.credentials ?? \"{}\") === \"omit\") {{",
            escape_js_string(&config.default_credentials)
        );
        w!(
            out,
            "{pad2}  throw new Error(`Route declared [ws][auth] but options.credentials is \"omit\"; refusing to send an unauthenticated upgrade ({})`);",
            escape_js_string(&route.name)
        );
        w!(out, "{pad2}}}");
    }

    if has_types {
        let large_int = config.large_int_type.as_str();
        let send_ts = rust_type_to_ts_with(route.ws_send_type.as_ref().unwrap(), large_int);
        let recv_ts = rust_type_to_ts_with(route.ws_receive_type.as_ref().unwrap(), large_int);
        w!(
            out,
            "{pad2}assertSecureTransport(url, {{ allowInsecureHttp: options.allowInsecureHttp === true, auth: {}, credentials: options.credentials ?? \"{}\" }});",
            route.is_credentialed(),
            escape_js_string(&config.default_credentials)
        );
        w!(out, "{pad2}const ws = new WebSocket(url);");
        w!(
            out,
            "{pad2}return createTypedWebSocket<{send_ts}, {recv_ts}>(ws);"
        );
    } else {
        w!(
            out,
            "{pad2}assertSecureTransport(url, {{ allowInsecureHttp: options.allowInsecureHttp === true, auth: {}, credentials: options.credentials ?? \"{}\" }});",
            route.is_credentialed(),
            escape_js_string(&config.default_credentials)
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

/// Removes the temp file on drop.
struct TempFile(std::path::PathBuf);

impl TempFile {
    /// Creates a unique temp file with `O_EXCL`; fails if the path exists.
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
                    // Collision (vanishingly unlikely): retry with a new name.
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

/// Returns `Ok(())` if generated output matches the committed file.
/// With `format_command`, formats a temp copy before comparing.
pub fn check(routes: &RouteCollection, config: &GeneratorConfig) -> Result<(), CheckError> {
    let generated = generate(routes, config);

    let expected = if let Some(ref cmd) = config.format_command {
        let temp = TempFile::create("axotyped_check", generated.as_bytes()).map_err(|e| {
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
        // Unterminated `${` must not survive as a live substitution across
        // the slice boundary in `build_path_template` (`$` at end of one
        // slice, `{evil` in the next). Escaped output contains `\${`, so check
        // for the exact escaped form (`\${` contains `${` as substring).
        assert_eq!(
            build_path_template("/x/${evil"),
            "`/x/\\${evil`",
            "unterminated dollar-brace must be escaped"
        );
        // Double-quoted branch escapes quotes/backslashes:
        assert_eq!(build_path_template("/a\"b"), "\"/a\\\"b\"");
    }

    #[test]
    fn test_build_path_template_escapes_dollar_without_params() {
        // Brace-free paths take the double-quoted branch, whose output is
        // re-embedded in template literals after stripping the quotes — so
        // `$` must be escaped even with no `{` in sight (`"\$"` equals `"$"`).
        assert_eq!(build_path_template("/x$evil"), "\"/x\\$evil\"");
        assert_eq!(build_path_template("/price$"), "\"/price\\$\"");
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
