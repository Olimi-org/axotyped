/// HTTP method for a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A path parameter extracted from a route path (e.g., `{id}` in `/users/{id}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathParam {
    pub name: String,
}

/// Definition of a single API route.
#[derive(Debug, Clone)]
pub struct RouteDefinition {
    /// Function name in the generated client (e.g., `register`, `listUsers`).
    pub name: String,
    /// HTTP method.
    pub method: HttpMethod,
    /// Route path (e.g., `/register`, `/admin/users/{id}`).
    pub path: String,
    /// Whether the route requires authentication.
    ///
    /// Defaults to `true`; `auth_layer` in scope forces `true`
    /// even for public-declared routes.
    pub auth: bool,
    /// Whether the route was declared public (`#[endpoint(public)]`,
    /// `[public]`, or a `group_public` scope). With `auth == true`,
    /// indicates an auth-layer override reported by `generate_with_warnings`.
    pub declared_public: bool,
    /// Rust type name of the request body (stringified via `stringify!()`).
    pub body_type: Option<String>,
    /// Rust type name of the response body (stringified via `stringify!()`).
    pub response_type: Option<String>,
    /// Rust type name for query parameters (stringified via `stringify!()`).
    pub query_type: Option<String>,
    /// Path parameters extracted from the path.
    pub path_params: Vec<PathParam>,
    /// Group name for nested object structure (e.g., `emailPassword`).
    pub group: Option<String>,
    /// Whether this route is a browser redirect (not a fetch call).
    pub redirect: bool,
    /// Whether this route is a WebSocket endpoint (generates WS connection, not fetch).
    pub websocket: bool,
    /// Rust type name for client-to-server events (send direction).
    pub ws_send_type: Option<String>,
    /// Rust type name for server-to-client events (receive direction).
    pub ws_receive_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Type collection (ts-rs feature)
// ---------------------------------------------------------------------------

/// A type-erased `T::export_all(&cfg)` function pointer.
/// Used to call ts-rs's export mechanism for types discovered during route building.
#[cfg(feature = "ts-rs")]
type ExportFn = fn(&crate::ts::Config) -> Result<(), crate::ts::ExportError>;

/// Called when a route names type `T` for TypeScript export.
///
/// [`NoCollect`] ignores registrations; [`TypeRegistry`] collects them
/// for binding generation.
pub trait Collector: Default {
    /// Register `T`. The default is a no-op; collecting collectors override this.
    fn register<T: crate::MaybeTs + 'static>(&mut self) {}

    /// Merge another collector of the same kind into this one (used by `merge` / `group_with`).
    fn merge_collection(&mut self, _other: Self) {}

    /// Consume the collector into the [`TypeRegistry`] it accumulated.
    fn into_type_registry(self) -> TypeRegistry;
}

/// Lean collector that registers nothing. The default for [`crate::ApiRouter`].
#[derive(Debug, Clone, Default)]
pub struct NoCollect;

impl Collector for NoCollect {
    fn into_type_registry(self) -> TypeRegistry {
        TypeRegistry::default()
    }
}

/// Collects route types for ts-rs export via `export_all()`.
/// Deduplicates by `TypeId` so shared generic impls emit one declaration.
#[cfg(feature = "ts-rs")]
#[derive(Debug, Clone, Default)]
pub struct TypeRegistry {
    /// TypeId → export_all function pointer, for deduplication and export.
    slots: Vec<(std::any::TypeId, ExportFn)>,
    seen: std::collections::BTreeSet<std::any::TypeId>,
}

/// Returns true for stdlib wrappers handled inline (`Vec` as `T[]`, `Option` as `T | null`).
#[cfg(feature = "ts-rs")]
fn is_container_wrapper(type_name: &str) -> bool {
    type_name.contains("::vec::Vec<") || type_name.contains("::option::Option<")
}

#[cfg(feature = "ts-rs")]
impl TypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a type has already been registered.
    pub fn contains_type<T: 'static>(&self) -> bool {
        self.seen.contains(&std::any::TypeId::of::<T>())
    }

    /// All registered export function pointers.
    pub fn slots(&self) -> &[(std::any::TypeId, ExportFn)] {
        &self.slots
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Merge another registry into this one. Duplicates are skipped.
    pub fn extend(&mut self, other: TypeRegistry) {
        for (type_id, export_fn) in other.slots {
            if self.seen.insert(type_id) {
                self.slots.push((type_id, export_fn));
            }
        }
    }
}

#[cfg(feature = "ts-rs")]
impl Collector for TypeRegistry {
    fn register<T: crate::MaybeTs + 'static>(&mut self) {
        let type_name = std::any::type_name::<T>();
        if is_container_wrapper(type_name) {
            return;
        }
        let type_id = std::any::TypeId::of::<T>();
        if self.seen.insert(type_id) {
            self.slots.push((type_id, T::export_all));
        }
    }

    fn merge_collection(&mut self, other: Self) {
        self.extend(other);
    }

    fn into_type_registry(self) -> TypeRegistry {
        self
    }
}

/// Placeholder registry when ts-rs is not enabled — no type collection.
#[cfg(not(feature = "ts-rs"))]
#[derive(Debug, Clone, Default)]
pub struct TypeRegistry;

#[cfg(not(feature = "ts-rs"))]
impl TypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        true
    }

    pub fn len(&self) -> usize {
        0
    }

    pub fn extend(&mut self, _other: TypeRegistry) {}
}

/// A collection of route definitions and their associated TypeScript types.
#[derive(Debug, Clone, Default)]
pub struct RouteCollection {
    routes: Vec<RouteDefinition>,
    types: TypeRegistry,
}

impl RouteCollection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a `RouteCollection` from routes and a type registry.
    /// Used by [`crate::ApiRouter::build`].
    pub(crate) fn assemble(routes: Vec<RouteDefinition>, types: TypeRegistry) -> Self {
        Self { routes, types }
    }

    pub fn push(&mut self, route: RouteDefinition) {
        self.routes.push(route);
    }

    pub fn extend(&mut self, other: RouteCollection) {
        self.routes.extend(other.routes);
        self.types.extend(other.types);
    }

    pub fn routes(&self) -> &[RouteDefinition] {
        &self.routes
    }

    pub fn types(&self) -> &TypeRegistry {
        &self.types
    }

    /// Exports collected types and dependencies to `dir` via `export_all()`.
    /// Call before `generate_to_file()`; replaces manual per-type exports.
    /// Types come from `.response::<T>()`, `.body::<T>()`, `.query::<T>()`, `.events::<A, B>()`.
    #[cfg(feature = "ts-rs")]
    pub fn export_types(&self, dir: &std::path::Path) -> Result<(), std::io::Error> {
        self.export_types_with(dir, crate::ts::Config::from_env())
    }

    /// Exports types with an explicit ts-rs [`Config`](crate::ts::Config).
    /// Use [`GeneratorConfig::ts_config`](crate::GeneratorConfig::ts_config) to keep
    /// binding and client integer rendering aligned.
    #[cfg(feature = "ts-rs")]
    pub fn export_types_with(
        &self,
        dir: &std::path::Path,
        cfg: crate::ts::Config,
    ) -> Result<(), std::io::Error> {
        use std::fs;

        if self.types.is_empty() {
            return Ok(());
        }

        fs::create_dir_all(dir)?;
        let cfg = cfg.with_out_dir(dir.to_path_buf());

        for (_, export_fn) in self.types.slots() {
            if let Err(e) = export_fn(&cfg) {
                // Log but don't fail — one bad type shouldn't block generation
                eprintln!("axotyped: type export failed: {e}");
            }
        }

        // Write a barrel index.ts that re-exports every .ts file in the
        // bindings directory.  ts-rs's export_all() also writes transitive
        // dependencies (e.g. DialogLine used inside Dialog), so we scan the
        // directory to pick up all generated types — not just the ones that
        // appear directly in route signatures.
        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("ts") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    // Skip index files to avoid circular references
                    if stem != "index" {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names.sort();

        if !names.is_empty() {
            let imports: Vec<String> = names
                .iter()
                .map(|n| format!("import type {{ {n} }} from \"./{n}\";"))
                .collect();
            let exports = names.join(", ");
            let barrel = format!(
                "// Auto-generated by axotyped. Do not edit.\n\n{}\n\nexport type {{ {} }};\n",
                imports.join("\n"),
                exports,
            );
            fs::write(dir.join("index.ts"), barrel)?;
        }

        Ok(())
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, RouteDefinition> {
        self.routes.iter()
    }
}

impl IntoIterator for RouteCollection {
    type Item = RouteDefinition;
    type IntoIter = std::vec::IntoIter<RouteDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.into_iter()
    }
}

impl<'a> IntoIterator for &'a RouteCollection {
    type Item = &'a RouteDefinition;
    type IntoIter = std::slice::Iter<'a, RouteDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.iter()
    }
}

/// Returns true if `name` can be emitted as-is in generated code.
/// ASCII identifier check plus reserved-word rejection; rejected names
/// become synthetic `__param_N` placeholders.
pub fn is_valid_js_identifier(name: &str) -> bool {
    // ECMAScript reserved words: strict keywords, future reserved words
    // (strict mode / modules), and contextual keywords that cannot serve as
    // plain bindings in generated signatures or module code.
    const RESERVED: &[&str] = &[
        // Strict keywords (always reserved)
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "new",
        "null",
        "return",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
        // Future reserved words in strict mode / modules
        "implements",
        "interface",
        "let",
        "package",
        "private",
        "protected",
        "public",
        "static",
        // Contextual keywords rejected conservatively
        "arguments",
        "async",
        "eval",
        "of",
    ];

    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') && !RESERVED.contains(&name)
}

/// Extracts `{...}` params from a path (e.g. `/users/{id}` -> `[id]`).
/// Repeats collapse to first occurrence; non-identifiers become `__param_N`,
/// matching path-template generation.
pub fn extract_path_params(path: &str) -> Vec<PathParam> {
    let raws = scan_raw_path_params(path);

    // Names claimed by valid raw identifiers; synthetics must avoid these.
    let taken: std::collections::BTreeSet<String> = raws
        .iter()
        .filter(|raw| is_valid_js_identifier(raw))
        .map(|raw| (*raw).to_string())
        .collect();

    let mut next_synthetic = 0usize;
    raws.into_iter()
        .enumerate()
        .map(|(index, raw)| PathParam {
            name: sanitize_param_name_with(&taken, &mut next_synthetic, raw, index),
        })
        .collect()
}

/// Returns raw `{...}` contents in first-occurrence order.
/// An unterminated `{` ends the scan; the remainder is literal text.
fn scan_raw_path_params(path: &str) -> Vec<&str> {
    let mut raws: Vec<&str> = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        match rest[open..].find('}') {
            Some(close_rel) => {
                let raw = &rest[open + 1..open + close_rel];
                if !raws.contains(&raw) {
                    raws.push(raw);
                }
                rest = &rest[open + close_rel + 1..];
            }
            None => break,
        }
    }
    raws
}

/// Raw names in `path` that are valid identifiers.
pub(crate) fn scan_taken_param_names(path: &str) -> std::collections::BTreeSet<String> {
    scan_raw_path_params(path)
        .into_iter()
        .filter(|raw| is_valid_js_identifier(raw))
        .map(String::from)
        .collect()
}

/// Maps a raw param name to its emitted name, allocating `__param_N`
/// for invalid identifiers. Shared by param extraction and template generation.
pub(crate) fn sanitize_param_name_with(
    taken: &std::collections::BTreeSet<String>,
    next_synthetic: &mut usize,
    raw: &str,
    _index: usize,
) -> String {
    if is_valid_js_identifier(raw) {
        return raw.to_string();
    }
    loop {
        let candidate = format!("__param_{next_synthetic}");
        *next_synthetic += 1;
        // Skip names already taken or equal to the raw input.
        if !taken.contains(&candidate) && candidate != raw {
            return candidate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_no_params() {
        assert!(extract_path_params("/register").is_empty());
        assert!(extract_path_params("/admin/users").is_empty());
    }

    #[test]
    fn extract_single_param() {
        let params = extract_path_params("/admin/users/{id}");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "id");
    }

    #[test]
    fn extract_multiple_params() {
        let params = extract_path_params("/orgs/{org_id}/users/{user_id}");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "org_id");
        assert_eq!(params[1].name, "user_id");
    }

    #[test]
    fn extract_inline_capture_with_suffix() {
        // axum accepts `{id}suffix`; the param must not vanish from the signature
        let params = extract_path_params("/u/{id}v2");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "id");
    }

    #[test]
    fn non_identifier_param_contents_become_synthetic_names() {
        let params = extract_path_params("/u/{x`; alert(document.cookie); y}");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "__param_0");
    }

    #[test]
    fn duplicate_params_collapse_to_first_occurrence() {
        let params = extract_path_params("/{a}/{b}/{a}");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "a");
        assert_eq!(params[1].name, "b");
    }

    #[test]
    fn js_identifier_validation() {
        assert!(is_valid_js_identifier("id"));
        assert!(is_valid_js_identifier("_private"));
        assert!(is_valid_js_identifier("$ref"));
        assert!(!is_valid_js_identifier("2fa"));
        assert!(!is_valid_js_identifier("a b"));
        assert!(!is_valid_js_identifier("class")); // reserved word
        assert!(!is_valid_js_identifier(""));
        assert!(!is_valid_js_identifier("a;evil()"));
    }

    #[test]
    fn route_collection_extend() {
        let mut a = RouteCollection::new();
        a.push(RouteDefinition {
            name: "foo".into(),
            method: HttpMethod::Get,
            path: "/foo".into(),
            auth: false,
            declared_public: false,
            body_type: None,
            response_type: None,
            query_type: None,
            path_params: vec![],
            group: None,
            redirect: false,
            websocket: false,
            ws_send_type: None,
            ws_receive_type: None,
        });

        let mut b = RouteCollection::new();
        b.push(RouteDefinition {
            name: "bar".into(),
            method: HttpMethod::Post,
            path: "/bar".into(),
            auth: true,
            declared_public: false,
            body_type: Some("BarRequest".into()),
            response_type: Some("BarResponse".into()),
            query_type: None,
            path_params: vec![],
            group: Some("baz".into()),
            redirect: false,
            websocket: false,
            ws_send_type: None,
            ws_receive_type: None,
        });

        a.extend(b);
        assert_eq!(a.len(), 2);
        assert_eq!(a.routes()[1].name, "bar");
    }
}
