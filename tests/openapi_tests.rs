//! OpenAPI composition end-to-end: real routes through the endpoint macro,
//! schema capture via the registry, paths composed from the walk.

#![cfg(feature = "openapi")]

use axotyped::register;
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
