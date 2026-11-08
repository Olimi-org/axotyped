//! OpenAPI composition end-to-end: real routes through the endpoint macro,
//! schema capture via the registry, paths composed from the walk.

#![cfg(feature = "openapi")]

use axotyped::register;
use axotyped::openapi::discriminate_serde_tagged_unions;
use utoipa::PartialSchema as _;
use utoipa::ToSchema as _;
use axotyped::openapi::{OpenApiConfig, SkippedRoute};
use axum::extract::{Json, Path, Query, State};
use serde::{Deserialize, Serialize};
use utoipa::openapi::{RefOr, Required, schema::Schema};

#[derive(Clone, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct TestState;

#[derive(Debug, Serialize, Deserialize, axotyped::TS, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JobInput {
    /// A unit variant; the tag is the whole payload.
    Ping,
    Enqueue { job_id: String },
}

#[derive(Debug, Serialize, Deserialize, axotyped::TS, utoipa::ToSchema)]
struct JobDone {
    job_id: String,
    attempts: u32,
    /// Absent vs null are distinct on the wire; Option drives tri-state.
    #[serde(skip_serializing_if = "Option::is_none")]
    log: Option<String>,
}

#[derive(Debug, Deserialize, axotyped::TS, utoipa::ToSchema)]
struct JobQuery {
    limit: Option<u32>,
    /// A required query param, flattened into a required Parameter.
    queue: String,
}

#[axotyped::endpoint]
async fn submit_job(
    State(_state): State<TestState>,
    Json(_body): Json<JobInput>,
) -> Json<JobDone> {
    Json(JobDone { job_id: String::new(), attempts: 0, log: None })
}

#[axotyped::endpoint]
async fn job_status(
    State(_state): State<TestState>,
    Path(_id): Path<String>,
    Query(_q): Query<JobQuery>,
) -> Json<JobDone> {
    Json(JobDone { job_id: String::new(), attempts: 0, log: None })
}

#[derive(Clone, Copy)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TestAuth;

impl axotyped::EndpointParam for TestAuth {
    type Auth = axotyped::Credential;
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for TestAuth {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(TestAuth)
    }
}

#[axotyped::endpoint]
async fn my_jobs(State(_state): State<TestState>, _auth: TestAuth) -> Json<Vec<JobDone>> {
    Json(Vec::new())
}

fn collection() -> axotyped::RouteCollection {
    axotyped::collect_routes(|r| {
        r.group_public("jobs", |g| {
            g.post("/jobs", register!(submit_job))
                .get("/jobs/{id}", register!(job_status))
                .as_("jobs")
        })
        .get("/mine", register!(my_jobs))
    })
}

fn document() -> (
    axotyped::openapi::OpenApiDocument,
    utoipa::openapi::schema::Components,
) {
    let routes = collection();
    let doc = routes.openapi_document(&OpenApiConfig::new("test", "0.1.0"));
    let components = doc.doc.components.clone().expect("components");
    (doc, components)
}

#[test]
fn components_capture_the_route_type_closure() {
    let (_doc, components) = document();
    for name in ["JobInput", "JobInputPing", "JobInputEnqueue", "JobDone", "JobQuery"] {
        assert!(components.schemas.contains_key(name), "{name} not captured");
    }
}

#[test]
fn paths_compose_with_shared_path_items_and_flattened_query() {
    let (doc, _components) = document();
    let paths = &doc.doc.paths;
    let jobs = paths.paths.get("/jobs").expect("/jobs");
    assert!(jobs.post.is_some(), "POST /jobs composed");

    let status = paths.paths.get("/jobs/{id}").expect("/jobs/{id}");
    let op = status.get.as_ref().expect("GET /jobs/{id}");
    let params = op.parameters.as_ref().expect("parameters");
    let path_param = params.iter().find_map(|p| match p {
        RefOr::T(parameter) if parameter.name == "id" => Some(parameter),
        _ => None,
    });
    assert!(path_param.is_some(), "path param composed");
    let query = params
        .iter()
        .filter_map(|p| match p {
            RefOr::T(parameter) if parameter.name == "queue" => Some(parameter),
            _ => None,
        })
        .next()
        .expect("required query param flattened");
    assert!(matches!(query.required, Required::True));
    params
        .iter()
        .filter_map(|p| match p {
            RefOr::T(parameter) if parameter.name == "limit" => Some(parameter),
            _ => None,
        })
        .for_each(|limit| assert!(matches!(limit.required, Required::False)));

    let mine = paths.paths.get("/mine").expect("/mine").get.as_ref().expect("GET /mine");
    assert!(mine.security.is_some(), "private route carries security");
    let submit = paths.paths.get("/jobs").expect("/jobs").post.as_ref().expect("POST /jobs");
    assert!(submit.security.is_none());
}

#[test]
fn tagged_union_is_discriminated_with_a_unit_variant_object() {
    let (doc, components) = document();
    assert_eq!(doc.skipped_routes.len(), 0, "all routes composed");
    let one_of = match &components.schemas["JobInput"] {
        RefOr::T(Schema::OneOf(one_of)) => one_of,
        other => panic!("JobInput is not a oneOf (is_ref={})", matches!(other, RefOr::Ref(_))),
    };
    let discriminator = one_of.discriminator.as_ref().expect("discriminator attached");
    assert_eq!(discriminator.property_name, "kind");
    assert_eq!(discriminator.mapping.get("ping").map(String::as_str), Some("#/components/schemas/JobInputPing"));

    match &components.schemas["JobInputPing"] {
        RefOr::T(Schema::Object(object)) => {
            assert_eq!(object.required, ["kind"]);
        }
        _ => panic!("JobInputPing not an object"),
    }
    let yaml = doc.to_yaml().expect("yaml");
    assert!(yaml.contains("discriminator"));
}

#[test]
fn generic_response_types_skip_their_routes_without_dangling_refs() {
    let routes: axotyped::RouteCollection = axotyped::collect_routes(|r| {
        r.group_public("jobs", |g| g.get("/missing", register!(job_status)).as_("jobs"))
    });
    // job_status returns JobDone, which is registered. Build the skip case by
    // composing with an empty component set.
    let mut empty_components = utoipa::openapi::schema::Components::default();
    let (paths, skipped) = routes.compose_paths(&mut empty_components);
    assert!(paths.paths.is_empty());
    let SkippedRoute { method, path, missing_type } = &skipped[0];
    assert_eq!(*method, "GET");
    assert_eq!(path, "/missing");
    // The walk checks query before response; JobQuery is the first missing type.
    assert_eq!(missing_type, "JobQuery");
    assert!(skipped.len() == 1, "one skip per route");
}

// ---------------------------------------------------------------------------
// Document composition regressions
// ---------------------------------------------------------------------------

#[axotyped::endpoint]
async fn heartbeat(State(_state): State<TestState>) {}

#[axotyped::endpoint]
async fn guarded_heartbeat(State(_state): State<TestState>, _auth: TestAuth) {}

#[axotyped::endpoint(opaque)]
async fn opaque_jobs(State(_state): State<TestState>) -> Json<Vec<JobDone>> {
    Json(Vec::new())
}

#[axotyped::endpoint]
async fn items_by_class(State(_state): State<TestState>, Path(_class): Path<String>) -> Json<JobDone> {
    Json(JobDone { job_id: String::new(), attempts: 0, log: None })
}

mod a {
    use super::*;
    #[axotyped::endpoint]
    pub async fn list(State(_state): State<TestState>) -> Json<Vec<JobDone>> {
        Json(Vec::new())
    }
}

mod b {
    use super::*;
    #[axotyped::endpoint]
    pub async fn list(State(_state): State<TestState>) -> Json<Vec<JobDone>> {
        Json(Vec::new())
    }
}

#[test]
fn no_body_routes_document_200_and_private_401() {
    let routes = axotyped::collect_routes(|r| {
        r.group_public("jobs", |g| g.get("/heartbeat", register!(heartbeat)).as_("heartbeat"))
            .get("/guarded", register!(guarded_heartbeat))
    });
    let doc = routes.openapi_document(&OpenApiConfig::new("t", "0"));
    let paths = &doc.doc.paths;

    let public = paths.paths["/heartbeat"].get.as_ref().expect("public no-body op");
    let ok = public
        .responses
        .responses
        .get("200")
        .expect("bare 200 documented for a unit handler");
    match ok {
        RefOr::T(response) => assert!(
            response.content.get("application/json").is_none(),
            "no body, no content"
        ),
        RefOr::Ref(_) => panic!("200 should not be a ref"),
    }

    let private = paths.paths["/guarded"].get.as_ref().expect("private no-body op");
    assert!(
        private.responses.responses.contains_key("401"),
        "private no-body route keeps its 401"
    );
}

#[test]
fn container_override_route_composes_with_explicit_schema_registration() {
    // Opaque handler + direct Vec override: the walk cannot name the item,
    // so the consumer registers it explicitly.
    let routes = axotyped::collect_routes(|r| {
        r.group_public("jobs", |g| {
            g.get("/jobs-opaque", register!(opaque_jobs))
                .response::<Vec<JobDone>>()
                .as_("jobs")
        })
    })
    .with_schema::<JobDone>();
    let doc = routes.openapi_document(&OpenApiConfig::new("t", "0"));
    assert!(
        doc.skipped_routes.is_empty(),
        "Vec override route must compose once the item is registered: {:?}",
        doc.skipped_routes
    );
    let op = doc.doc.paths.paths["/jobs-opaque"].get.as_ref().expect("op");
    let response = match op.responses.responses.get("200").expect("200") {
        RefOr::T(response) => response,
        RefOr::Ref(_) => panic!("200 is a ref"),
    };
    let content = response.content.get("application/json").expect("json content");
    let schema = match content {
        RefOr::T(content) => content.schema.as_ref().expect("response schema"),
        RefOr::Ref(_) => panic!("content is a bare ref"),
    };
    let RefOr::T(Schema::Array(_)) = schema else {
        panic!("Vec response must compose as an array");
    };
    assert!(doc.doc.components.expect("components").schemas.contains_key("JobDone"));
}

#[test]
fn colliding_variant_names_leave_the_union_undiscriminated() {
    #[derive(Debug, Serialize, Deserialize, axotyped::TS, utoipa::ToSchema)]
    #[serde(tag = "shape", rename_all = "snake_case")]
    enum Mixed {
        #[serde(rename = "foo-bar")]
        FooBar { a: String },
        #[serde(rename = "foo_bar")]
        FooBar2 { b: String },
    }

    let mut doc = utoipa::openapi::OpenApi::default();
    let mut captured: Vec<(String, RefOr<Schema>)> = Vec::new();
    <Mixed as utoipa::ToSchema>::schemas(&mut captured);
    // The transform reads oneOf schemas, so the union registers itself beside its variants.
    captured.push((<Mixed as utoipa::ToSchema>::name().to_string(), <Mixed as utoipa::PartialSchema>::schema()));
    let mut components = utoipa::openapi::schema::Components::default();
    for (name, schema) in captured {
        components.schemas.entry(name).or_insert(schema);
    }
    doc.components = Some(components);

    assert_eq!(
        discriminate_serde_tagged_unions(&mut doc),
        0,
        "foo-bar/foo_bar collide under pascal_case — union must stay unchanged"
    );
    let schemas = &doc.components.expect("components").schemas;
    let one_of = match &schemas["Mixed"] {
        RefOr::T(Schema::OneOf(one_of)) => one_of,
        other => panic!("Mixed is not a oneOf (is_ref={})", matches!(other, RefOr::Ref(_))),
    };
    assert!(one_of.discriminator.is_none(), "no discriminator without safe names");
}

#[test]
fn duplicate_names_across_groups_get_unique_stable_operation_ids() {
    let routes = axotyped::collect_routes(|r| {
        r.group_public("a", |g| g.get("/a/list", register!(a::list)).as_("list"))
            .group_public("b", |g| g.get("/b/list", register!(b::list)).as_("list"))
            .group_public("top", |g| g.get("/top", register!(items_by_class)).as_("top"))
    });
    let doc = routes.openapi_document(&OpenApiConfig::new("t", "0"));
    let mut ids: Vec<String> = Vec::new();
    for item in doc.doc.paths.paths.values() {
        for op in [&item.get, &item.post, &item.put, &item.patch, &item.delete].into_iter().flatten() {
            ids.push(op.operation_id.clone().expect("operation id"));
        }
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "operation ids must be unique: {ids:?}");
    // Disambiguation is symmetric: both colliding names group-qualify, so no
    // occurrence is privileged; unique names stay verbatim.
    assert!(ids.contains(&"a_list".to_string()), "{ids:?}");
    assert!(ids.contains(&"b_list".to_string()), "{ids:?}");
    assert!(ids.contains(&"top".to_string()), "unique names stay verbatim: {ids:?}");
}

#[test]
fn reserved_word_path_param_uses_the_template_name() {
    let routes = axotyped::collect_routes(|r| {
        r.group_public("jobs", |g| g.get("/items/{class}", register!(items_by_class)).as_("items"))
    });
    let doc = routes.openapi_document(&OpenApiConfig::new("t", "0"));
    let op = doc.doc.paths.paths["/items/{class}"].get.as_ref().expect("op");
    let params = op.parameters.as_ref().expect("parameters");
    let names: Vec<&str> = params
        .iter()
        .filter_map(|p| match p {
            RefOr::T(parameter) => Some(parameter.name.as_str()),
            RefOr::Ref(_) => None,
        })
        .collect();
    assert_eq!(names, ["class"], "parameter name must match the template, not the JS-safe alias");
}
