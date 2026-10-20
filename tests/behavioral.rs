//! Behavioral harness for the emitted client runtime.
//!
//! Generates the real client for a fixture pair-flow API into a temp dir,
//! copies [`tests/behavioral/spec.test.mjs`] next to it, and runs it under
//! `bun test`. The client spec executes the actual emitted client end to
//! end — the declaration as data, the generated renewal helper, the factory,
//! and the `auth` namespace — against an injected fetch.
//!
//! The runtime spec runs the shared executor directly against
//! `src/runtime.ts`, with no generation involved: descriptor
//! interpretation, transport policy, codecs, the security guards, and the
//! bounded replay contract all run as real code.
//!
//! Skips loudly (passing) when bun is not on PATH, so JS-runtime-less
//! environments still get `cargo test` green; run locally with
//! `cargo test --test behavioral`.

#![allow(dead_code)]

use axotyped::{AuthScheme, GeneratorConfig};

// --- fixture: an oauth2 pair flow with the full machinery -------------------

#[derive(serde::Serialize, axotyped::TS)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u32>,
}

#[axotyped::endpoint]
async fn exchange_tokens() -> axum::Json<TokenResponse> {
    axum::Json(TokenResponse {
        access_token: String::new(),
        refresh_token: None,
        expires_in: None,
    })
}

#[axotyped::credential]
#[derive(Clone, Copy)]
pub struct TestAuth;

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
async fn private_thing(_auth: TestAuth) {}

// --- fixture: a bearer-kind flow (engine persistence) ------------------------

#[derive(serde::Serialize, axotyped::TS)]
struct MeResponse {
    authenticated: bool,
    token: Option<String>,
}

#[axotyped::endpoint(permissive)]
async fn me() -> axum::Json<MeResponse> {
    axum::Json(MeResponse {
        authenticated: false,
        token: None,
    })
}

axotyped::auth_scheme! {
    pub SessionAuth {
        kind: exchange,
        exchange: register!(me) -> token,
        refresh: register!(me) -> token,
    }
}

axotyped::auth_scheme! {
    pub OAuthLogin {
        kind: oauth2,
        exchange: register!(exchange_tokens) -> { access: access_token, refresh: refresh_token, expires_in: expires_in },
        refresh: register!(exchange_tokens) -> { access: access_token, refresh: refresh_token, expires_in: expires_in },
    }
}

#[test]
fn behavioral_spec() {
    let bun = std::process::Command::new("bun")
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(&bun, Ok(st) if st.success()) {
        eprintln!("SKIPPED: behavioral spec needs bun on PATH (found: {bun:?})");
        return;
    }

    // The integration spec runs the real generated client (both files, via
    // generate_to_file). Engine/executor semantics run directly against
    // src/runtime.ts in runtime.spec.test.mjs — no generation involved.
    let dir = std::env::temp_dir().join(format!("axotyped-behavioral-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let (_r2, bearer_collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/auth/me", axotyped::register!(me))
        .get("/private", axotyped::register!(private_thing))
        .build();
    axotyped::generate_to_file(
        &bearer_collection,
        &GeneratorConfig {
            auth_scheme: AuthScheme::Bearer,
            output_path: dir.join("index.ts").to_string_lossy().into(),
            ..Default::default()
        },
    )
    .expect("generate client + runtime");

    let output = std::process::Command::new("bun")
        .args([
            "test",
            "tests/behavioral/runtime.spec.test.mjs",
            "tests/behavioral/client.spec.test.mjs",
        ])
        .env("AXOTYPED_CLIENT_DIR", dir.as_os_str())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run bun test");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Keep the temp dir only on failure, for inspection.
    if output.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
    }

    // bun test writes its results to stderr.
    let summary = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| l.contains("pass") || l.contains("fail") || l.contains("Ran"))
        .collect::<Vec<_>>()
        .join(" | ");
    eprintln!("behavioral spec: {summary}");

    assert!(
        output.status.success(),
        "behavioral spec failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}\n--- dir: {} ---",
        dir.display()
    );
}
