//! OpenAPI document composition from route metadata.
//!
//! The same [`RouteCollection`] that feeds the TypeScript client generator
//! feeds this: paths compose from [`RouteDefinition`]s, components from the
//! schema-capturing registry (`register_schema`, fed by the endpoint macro
//! beside the TS registrations), security from route visibility. utoipa is
//! the document model; enabling the feature makes `ToSchema` a compile-time
//! requirement on every route-referenced type, so the spec cannot name a type
//! the API doesn't schema.
//!
//! One transform is applied on top of utoipa's derive output: serde tagged
//! enums come out as `oneOf` over named variant schemas plus a
//! [`discriminator`], or schema consumers cannot tell the variants apart
//! (utoipa inlines them). The detection is purely shape-driven, with no
//! per-type knowledge.
//!
//! [discriminator]: utoipa::openapi::schema::Discriminator

use std::collections::BTreeMap;

use utoipa::openapi::{
    OpenApi, RefOr, Required,
    path::{OperationBuilder, ParameterBuilder, ParameterIn, Paths},
    request_body::RequestBody,
    response::{Response, Responses},
    schema::{Components, Discriminator, Object, OneOf, Ref, Schema, SchemaType, Type},
    security::{Http as HttpAuth, HttpAuthScheme, HttpBuilder as HttpAuthBuilder, SecurityRequirement, SecurityScheme},
    server::Server,
};

use crate::types::{HttpMethod, RouteCollection, RouteDefinition, Visibility};

/// Name of the bearer security scheme the composed operations reference.
pub const BEARER_SCHEME: &str = "bearerAuth";

/// A route omitted from `paths` when a referenced type has no component
/// (a generic instantiation, typically). The walk emits no dangling `$ref`
/// for skipped routes.
#[derive(Debug, Clone)]
pub struct SkippedRoute {
    pub method: &'static str,
    pub path: String,
    /// The type name that failed to resolve.
    pub missing_type: String,
}

/// Inputs an OpenAPI document needs beyond route metadata.
#[derive(Debug, Clone)]
pub struct OpenApiConfig {
    pub title: String,
    pub version: String,
    /// Declared server base URL (documentation only; the generated client
    /// takes its base URL at construction).
    pub server: Option<String>,
}

impl OpenApiConfig {
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self { title: title.into(), version: version.into(), server: None }
    }

    pub fn server(mut self, url: impl Into<String>) -> Self {
        self.server = Some(url.into());
        self
    }
}

/// The composed document and the routes skipped during composition.
#[derive(Clone)]
pub struct OpenApiDocument {
    pub doc: OpenApi,
    pub skipped_routes: Vec<SkippedRoute>,
}

impl OpenApiDocument {
    /// Serialize to YAML. The error is stringified; a serialization failure
    /// indicates a configuration bug.
    pub fn to_yaml(&self) -> Result<String, String> {
        self.doc.to_yaml().map_err(|e| e.to_string())
    }
}

impl RouteCollection {
    /// Assemble the full OpenAPI document: components from the schema registry
    /// (transitive closure via `ToSchema::schemas`), paths from the route
    /// walk, bearer security from route visibility, with serde tagged enums
    /// discriminated.
    pub fn openapi_document(&self, config: &OpenApiConfig) -> OpenApiDocument {
        let mut components = Components::default();
        let mut captured: Vec<(String, RefOr<Schema>)> = Vec::new();
        for (_, schema_fn) in self.types().schema_slots() {
            schema_fn(&mut captured);
        }
        for (name, schema) in captured {
            components.schemas.entry(name).or_insert(schema);
        }
        components
            .security_schemes
            .insert(BEARER_SCHEME.into(), RefOr::T(SecurityScheme::Http(http_bearer())));

        let (paths, skipped_routes) = compose_paths(self, &mut components);

        let mut doc = OpenApi::default();
        doc.info.title = config.title.clone();
        doc.info.version = config.version.clone();
        doc.components = Some(components);
        doc.paths = paths;
        if let Some(server) = &config.server {
            doc.servers = Some(vec![Server::new(server)]);
        }
        discriminate_serde_tagged_unions(&mut doc);

        OpenApiDocument { doc, skipped_routes }
    }

    /// Compose only the paths object, for consumers assembling a document
    /// from a hand-written component list. `components` must already carry
    /// the schemas the routes name; the bearer scheme is inserted.
    pub fn compose_paths(&self, components: &mut Components) -> (Paths, Vec<SkippedRoute>) {
        compose_paths(self, components)
    }
}

/// Walk the route collection into OpenAPI paths. Several routes may share one
/// path (POST+GET+DELETE on the same URL); each contributes its method to
/// the same `PathItem`.
pub fn compose_paths(
    routes: &RouteCollection,
    components: &mut Components,
) -> (Paths, Vec<SkippedRoute>) {
    components
        .security_schemes
        .insert(BEARER_SCHEME.into(), RefOr::T(SecurityScheme::Http(http_bearer())));

    let mut paths = Paths::new();
    let mut skipped = Vec::new();
    for route in routes.routes() {
        if route.websocket || route.redirect {
            continue;
        }
        let method = &route.method;
        let skip = |missing: String| SkippedRoute {
            method: method.as_str(),
            path: route.path.clone(),
            missing_type: missing,
        };
        let oa_method = match method {
            HttpMethod::Get => utoipa::openapi::path::HttpMethod::Get,
            HttpMethod::Put => utoipa::openapi::path::HttpMethod::Put,
            HttpMethod::Post => utoipa::openapi::path::HttpMethod::Post,
            HttpMethod::Patch => utoipa::openapi::path::HttpMethod::Patch,
            HttpMethod::Delete => utoipa::openapi::path::HttpMethod::Delete,
        };

        let mut parameters = path_parameters(route);
        if let Some(query_type) = route.query_type.as_deref() {
            match query_parameters(query_type, components) {
                Some(query_params) => parameters.extend(query_params),
                None => {
                    skipped.push(skip(query_type.to_string()));
                    continue;
                }
            }
        }

        let mut operation = OperationBuilder::new()
            .operation_id(Some(route.name.clone()))
            .parameters(Some(parameters));

        if let Some(body_type) = route.body_type.as_deref() {
            match resolve_schema_ref(body_type, components) {
                Some(schema) => {
                    let mut request_body = RequestBody::new();
                    request_body
                        .content
                        .insert("application/json".into(), json_content(schema));
                    request_body.required = Some(Required::True);
                    operation = operation.request_body(Some(request_body));
                }
                None => {
                    skipped.push(skip(body_type.to_string()));
                    continue;
                }
            }
        }

        if let Some(response_type) = route.response_type.as_deref() {
            match resolve_schema_ref(response_type, components) {
                Some(schema) => {
                    let mut ok = Response::new("OK");
                    ok.content.insert("application/json".into(), json_content(schema));
                    let mut responses = Responses::new();
                    responses.responses.insert("200".into(), RefOr::T(ok));
                    if route.visibility == Visibility::Private {
                        responses.responses.insert(
                            "401".into(),
                            RefOr::T(Response::new(
                                "Session expired — renew and retry once",
                            )),
                        );
                    }
                    operation = operation.responses(responses);
                }
                None => {
                    skipped.push(skip(response_type.to_string()));
                    continue;
                }
            }
        }

        if route.visibility == Visibility::Private {
            operation = operation.security(SecurityRequirement::new(BEARER_SCHEME, [] as [String; 0]));
        }
        if let Some(group) = &route.group {
            operation = operation.tags(Some([group.clone()]));
        }

        let entry = paths.paths.entry(route.path.clone()).or_default();
        match oa_method {
            utoipa::openapi::path::HttpMethod::Get => entry.get = Some(operation.build()),
            utoipa::openapi::path::HttpMethod::Put => entry.put = Some(operation.build()),
            utoipa::openapi::path::HttpMethod::Post => entry.post = Some(operation.build()),
            utoipa::openapi::path::HttpMethod::Patch => entry.patch = Some(operation.build()),
            utoipa::openapi::path::HttpMethod::Delete => entry.delete = Some(operation.build()),
            _ => {}
        }
    }
    (paths, skipped)
}

fn http_bearer() -> HttpAuth {
    HttpAuthBuilder::default()
        .scheme(HttpAuthScheme::Bearer)
        .bearer_format("JWT")
        .build()
}

fn path_parameters(route: &RouteDefinition) -> Vec<utoipa::openapi::path::Parameter> {
    route
        .path_params
        .iter()
        .map(|param| {
            ParameterBuilder::default()
                .name(&param.name)
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(RefOr::T(Schema::Object(string_schema()))))
                .build()
        })
        .collect()
}

fn string_schema() -> Object {
    let mut object = Object::new();
    object.schema_type = SchemaType::Type(Type::String);
    object
}

/// Flatten a registered query struct's schema into one Query parameter per
/// property. Names, types, required flags, and enum values come from the
/// schema itself.
fn query_parameters(query_type: &str, components: &Components) -> Option<Vec<utoipa::openapi::path::Parameter>> {
    let RefOr::T(Schema::Object(object)) = components.schemas.get(query_type)? else {
        return None;
    };
    Some(
        object
            .properties
            .iter()
            .map(|(name, schema)| {
                ParameterBuilder::default()
                    .name(name)
                    .parameter_in(ParameterIn::Query)
                    .required(if object.required.iter().any(|r| r == name) {
                        Required::True
                    } else {
                        Required::False
                    })
                    .schema(Some(schema.clone()))
                    .build()
            })
            .collect(),
    )
}

/// Map `Vec<X>` to an array of `$ref` and `X` to a `$ref`. Route metadata
/// records short names (`type_string` strips module paths) matching the
/// component keys `ToSchema::name()` registers. Returns `None` for anything
/// else (generic instantiations, aliases); the caller skips the route
/// instead of emitting a dangling reference.
fn resolve_schema_ref(type_name: &str, components: &Components) -> Option<RefOr<Schema>> {
    if let Some(inner) = type_name.strip_prefix("Vec<").and_then(|s| s.strip_suffix('>')) {
        if !components.schemas.contains_key(inner) {
            return None;
        }
        return Some(RefOr::T(Schema::Array(utoipa::openapi::schema::Array::new(
            RefOr::Ref(Ref::from_schema_name(inner)),
        ))));
    }
    if components.schemas.contains_key(type_name) {
        Some(RefOr::Ref(Ref::from_schema_name(type_name)))
    } else {
        None
    }
}

/// A request body and a JSON response share the same content shape.
fn json_content(schema: RefOr<Schema>) -> RefOr<utoipa::openapi::Content> {
    RefOr::T(utoipa::openapi::Content::new(Some(schema)))
}

/// Hoist utoipa's inline `oneOf` variants of serde tagged enums to named
/// component schemas and attach the discriminator. Purely shape-driven: a
/// `oneOf` whose branches are all inline objects sharing exactly one required
/// single-value string-enum property is a tagged enum, and that property is
/// the tag. Returns how many unions were discriminated.
pub fn discriminate_serde_tagged_unions(doc: &mut OpenApi) -> usize {
    let Some(components) = doc.components.as_mut() else {
        return 0;
    };

    // Replacements are collected first: the map is borrowed for reading above.
    let mut replacements: Vec<(String, OneOf)> = Vec::new();
    let mut added: Vec<(String, RefOr<Schema>)> = Vec::new();
    let mut discriminated = 0;

    let names: Vec<String> = components.schemas.keys().cloned().collect();
    for name in names {
        let Some(RefOr::T(Schema::OneOf(one_of))) = components.schemas.get(&name) else {
            continue;
        };
        if one_of.discriminator.is_some() {
            continue;
        }
        let Some(tag) = shared_tag(one_of) else {
            continue;
        };

        let mut items = Vec::with_capacity(one_of.items.len());
        let mut mapping = BTreeMap::new();
        let mut complete = true;
        for branch in &one_of.items {
            let RefOr::T(Schema::Object(object)) = branch else {
                complete = false;
                break;
            };
            let Some(value) = tag_value(object, &tag) else {
                complete = false;
                break;
            };
            let variant = format!("{name}{}", pascal_case(&value));
            let location = format!("#/components/schemas/{variant}");
            items.push(RefOr::Ref(Ref::from_schema_name(&variant)));
            added.push((variant, branch.clone()));
            mapping.insert(value, location);
        }

        if !complete {
            continue; // Not a taggable object; leave the union unchanged.
        }
        let mut rewritten = one_of.clone();
        rewritten.items = items;
        rewritten.discriminator = Some(Discriminator::with_mapping(tag, mapping));
        replacements.push((name, rewritten));
        discriminated += 1;
    }

    for (name, one_of) in replacements {
        components.schemas.insert(name, RefOr::T(Schema::OneOf(one_of)));
    }
    for (name, schema) in added {
        components.schemas.entry(name).or_insert(schema);
    }
    discriminated
}

/// Return the shared tag property name when every branch is an inline object
/// with exactly one required single-value string-enum property, the same one
/// in all branches.
fn shared_tag(one_of: &OneOf) -> Option<String> {
    let mut branches = one_of.items.iter();
    let first = match branches.next()? {
        RefOr::T(Schema::Object(object)) => single_tag(object),
        _ => return None,
    }?;
    if branches.all(|branch| {
        matches!(branch, RefOr::T(Schema::Object(object)) if single_tag(object).as_deref() == Some(first.as_str()))
    }) {
        Some(first)
    } else {
        None
    }
}

fn single_tag(object: &Object) -> Option<String> {
    let mut tags: Vec<&String> = object
        .properties
        .iter()
        .filter(|(name, prop)| {
            object.required.iter().any(|r| r == *name)
                && matches!(
                    prop,
                    RefOr::T(Schema::Object(o))
                        if matches!(o.schema_type, SchemaType::Type(Type::String))
                            && o.enum_values.as_deref().is_some_and(|v| v.len() == 1)
                )
        })
        .map(|(name, _)| name)
        .collect();
    if tags.len() == 1 {
        Some(tags.remove(0).clone())
    } else {
        None
    }
}

fn tag_value(object: &Object, tag: &str) -> Option<String> {
    match object.properties.get(tag)? {
        RefOr::T(Schema::Object(o)) => match o.enum_values.as_deref() {
            Some([serde_json::Value::String(value)]) => Some(value.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn pascal_case(value: &str) -> String {
    value
        .split(['-', '_'])
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}
