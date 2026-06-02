//! Akribes SDK for Rust — typed client for the Akribes workflow platform.
//!
//! # Quick start
//!
//! ```no_run
//! use akribes_sdk::AkribesClient;
//!
//! # async fn example() -> akribes_sdk::Result<()> {
//! let client = AkribesClient::builder("http://localhost:3001")
//!     .project_id(1)
//!     .token("akribes_tk_my_api_key")
//!     .build();
//!
//! // Run a script and wait for the result
//! let (id, output) = client.project(1).executions().run("my-script")
//!     .channel("production")
//!     .execute_and_await(None).await?;
//!
//! println!("Result: {:?}", output.result);
//! # Ok(())
//! # }
//! ```

mod client;
pub mod error;
pub mod events;
pub mod models;
pub mod runtime;
pub mod sub;
pub mod suspend;
pub mod task_end;
pub mod token_safety;

// Re-export the main types at the crate root for convenience.
pub use client::{AkribesClient, AkribesClientBuilder};
pub use error::{AkribesError, InputValidationEntry, Result, parse_input_validation_errors};
pub use events::{EnvelopeDecodeError, EventCategory, WorkflowEvent};
pub use models::*;
pub use runtime::{
    RuntimeEndPayload, RuntimeErrorKind, RuntimeErrorPayload, RuntimeEvent, RuntimeStartPayload,
    RuntimeStderrPayload, RuntimeStdoutPayload,
};
pub use suspend::{SuspendTrigger, UnableRecord, ValidationErrorWire};
pub use task_end::TaskEndVariant;

// Re-export sub-clients for direct use.
pub use sub::bench::{BenchClient, BenchRunsClient};
pub use sub::channels::ChannelsClient;
pub use sub::clients::RegisteredClientsClient;
pub use sub::convert::ConvertClient;
pub use sub::documents::DocumentsClient;
pub use sub::drafts::DraftsClient;
pub use sub::events::EventsClient;
pub use sub::executions::ExecutionsClient;
pub use sub::projects::ProjectsClient;
pub use sub::run_stream::{
    EngineErrorPayload, RunStream, RunSummary, RunSummaryCost, RunSummaryDuration, RunSummaryTasks,
    SuspendPayload, TaskEndPayload,
};
pub use sub::tokens::TokensClient;

/// Test-only helpers, public so integration tests in `tests/` can reach
/// crate-internal constructors. Not a stable API — may change or disappear
/// without notice.
#[doc(hidden)]
pub mod _test {
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    /// Build a [`crate::RunStream`] from a raw receiver and join handle.
    /// Used by `tests/run_stream.rs` to exercise terminal detection without
    /// spinning up a mock SSE server.
    pub fn make_run_stream(
        execution_id: String,
        rx: mpsc::UnboundedReceiver<crate::error::Result<crate::events::WorkflowEvent>>,
        handle: JoinHandle<()>,
    ) -> crate::RunStream {
        let sub = crate::sub::events::EventSubscription::from_handle(handle);
        crate::sub::run_stream::RunStream::new(execution_id, rx, sub)
    }
}
pub use sub::scripts::ScriptsClient;
pub use sub::versions::{PublishBuilder, VersionsClient};

// Compile-time guarantees.
#[allow(dead_code)]
const _: () = {
    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
    fn checks() {
        assert_send_sync_clone::<AkribesClient>();
    }
};

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[test]
    fn client_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AkribesClient>();
        assert_send_sync::<crate::sub::executions::ExecutionsClient>();
        assert_send_sync::<crate::sub::scripts::ScriptsClient>();
        assert_send_sync::<crate::sub::tokens::TokensClient>();
    }

    fn make_client(server: &Server) -> AkribesClient {
        AkribesClient::builder(server.url())
            .project_id(1)
            .name("test-app")
            .id("test-id")
            .build()
    }

    fn make_authed_client(server: &Server) -> AkribesClient {
        AkribesClient::builder(server.url())
            .project_id(1)
            .name("test-app")
            .id("test-id")
            .token("test-token-123")
            .build()
    }

    // ── helpers for common JSON fixtures ─────────────────────────────────────

    fn project_json() -> &'static str {
        r#"{"id":10,"name":"My Project","created_at":"2024-01-01T00:00:00Z"}"#
    }

    fn script_json() -> &'static str {
        r#"{"id":5,"project_id":1,"name":"my_script","created_at":"2024-01-01T00:00:00Z"}"#
    }

    fn version_json() -> &'static str {
        r#"{"id":3,"script_id":5,"source":"workflow main {}","label":null,"published_by":null,"created_at":"2024-01-01T00:00:00Z"}"#
    }

    fn channel_json() -> &'static str {
        r#"{"id":1,"script_id":5,"name":"production","version_id":3,"updated_at":"2024-01-01T00:00:00Z"}"#
    }

    // ── auth ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_auth_header_sent() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .match_header("authorization", "Bearer test-token-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = make_authed_client(&server);
        client.projects().list().await.unwrap();
    }

    #[tokio::test]
    async fn test_no_auth_header_without_token() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = make_client(&server);
        client.projects().list().await.unwrap();
    }

    #[tokio::test]
    async fn test_set_token_updates_header() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .match_header("authorization", "Bearer new-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = make_client(&server);
        client.set_token(Some("new-token".to_string())).await;
        client.projects().list().await.unwrap();
    }

    #[tokio::test]
    async fn test_on_behalf_of_header_via_builder() {
        // Builder-set on_behalf_of must land on outbound requests as
        // X-Akribes-User. Mockito only matches the request when the header is
        // present with the expected value.
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .match_header("x-akribes-user", "alice@acme.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = AkribesClient::builder(server.url())
            .project_id(1)
            .name("test-app")
            .id("test-id")
            .on_behalf_of("alice@acme.com")
            .build();
        client.projects().list().await.unwrap();
    }

    #[tokio::test]
    async fn test_set_on_behalf_of_updates_header_at_runtime() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .match_header("x-akribes-user", "bob@acme.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = make_client(&server);
        client
            .set_on_behalf_of(Some("bob@acme.com".to_string()))
            .await;
        client.projects().list().await.unwrap();
    }

    #[tokio::test]
    async fn test_set_on_behalf_of_none_clears_header() {
        // Clearing the value via `set_on_behalf_of(None)` must not emit the
        // header on subsequent requests. mockito's `match_header` only fires
        // when the header is missing, so the request will only match if the
        // SDK truly omits it.
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .match_header("x-akribes-user", mockito::Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = AkribesClient::builder(server.url())
            .project_id(1)
            .on_behalf_of("alice@acme.com")
            .build();
        client.set_on_behalf_of(None).await;
        client.projects().list().await.unwrap();
    }

    // ── builder ───────────────────────────────────────────────────────────────

    #[test]
    fn test_builder_defaults() {
        let client = AkribesClient::builder("http://localhost:3001/")
            .project_id(42)
            .build();
        assert_eq!(client.inner.base_url, "http://localhost:3001");
        assert_eq!(client.project_id(), Some(42));
        assert_eq!(client.inner.name, "rust-sdk");
        assert!(!client.inner.id.is_empty()); // auto UUID
    }

    #[test]
    fn test_builder_custom() {
        let client = AkribesClient::builder("http://localhost:3001")
            .project_id(1)
            .name("my-svc")
            .id("custom-id")
            .token("tok")
            .build();
        assert_eq!(client.inner.name, "my-svc");
        assert_eq!(client.inner.id, "custom-id");
    }

    #[test]
    fn test_builder_no_project_id() {
        let client = AkribesClient::builder("http://localhost:3001").build();
        assert_eq!(client.project_id(), None);
    }

    #[test]
    fn test_client_is_clone() {
        let client = AkribesClient::builder("http://localhost:3001")
            .project_id(1)
            .name("test")
            .id("test")
            .build();
        let clone = client.clone();
        assert_eq!(clone.project_id(), Some(1));
    }

    // ── lifecycle ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_init() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/clients")
            .with_status(200)
            .with_body(r#"{"status":"ok"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .registered_clients()
                .init(vec![])
                .await
                .is_ok()
        );
        client.project(1).registered_clients().destroy();
    }

    #[tokio::test]
    async fn test_init_registers_heartbeat_and_destroy_cancels() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/clients")
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;

        let client = make_client(&server);
        client
            .project(1)
            .registered_clients()
            .init(vec![])
            .await
            .unwrap();
        assert!(client.inner.heartbeat_handle.lock().unwrap().is_some());
        client.project(1).registered_clients().destroy();
        assert!(client.inner.heartbeat_handle.lock().unwrap().is_none());
    }

    // ── projects ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_projects() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", project_json());
        let _m = server
            .mock("GET", "/projects")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        let projects = client.projects().list().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, 10);
        assert_eq!(projects[0].name, "My Project");
    }

    #[tokio::test]
    async fn test_get_project() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/10")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(project_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let proj = client.projects().get(10).await.unwrap().unwrap();
        assert_eq!(proj.id, 10);
    }

    #[tokio::test]
    async fn test_get_project_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/999")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(client.projects().get(999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_create_project() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(project_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let proj = client.projects().create("My Project").await.unwrap();
        assert_eq!(proj.id, 10);
    }

    #[tokio::test]
    async fn test_update_project() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PATCH", "/projects/10")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(project_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let proj = client.projects().update(10, "My Project").await.unwrap();
        assert_eq!(proj.id, 10);
    }

    #[tokio::test]
    async fn test_delete_project() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/10")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(client.projects().delete(10).await.is_ok());
    }

    #[tokio::test]
    async fn test_duplicate_project() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/10/duplicate")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(project_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let proj = client.projects().duplicate(10).await.unwrap();
        assert_eq!(proj.id, 10);
    }

    #[tokio::test]
    async fn test_reorder_projects() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PUT", "/projects/reorder")
            .match_body(r#"{"order":[3,1,2]}"#)
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        client.projects().reorder(vec![3, 1, 2]).await.unwrap();
    }

    // ── scripts ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_scripts() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", script_json());
        let _m = server
            .mock("GET", "/projects/1/scripts")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        let scripts = client.project(1).scripts().list().await.unwrap();
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].name, "my_script");
    }

    #[tokio::test]
    async fn test_create_script() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/scripts?name=my_script")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(script_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client
            .project(1)
            .scripts()
            .create("my_script", "workflow main {}")
            .await
            .unwrap();
        assert_eq!(s.name, "my_script");
    }

    #[tokio::test]
    async fn test_get_script() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(script_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client.project(1).scripts().get("my_script").await.unwrap();
        assert!(s.is_some());
        assert_eq!(s.unwrap().id, 5);
    }

    #[tokio::test]
    async fn test_get_script_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/missing")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client.project(1).scripts().get("missing").await.unwrap();
        assert!(s.is_none());
    }

    #[tokio::test]
    async fn test_rename_script() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PATCH", "/projects/1/scripts/old")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .scripts()
                .rename("old", "new")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_delete_script() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/1/scripts/my_script")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .scripts()
                .delete("my_script")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_duplicate_script() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/scripts/foo/duplicate")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(script_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client.project(1).scripts().duplicate("foo").await.unwrap();
        assert_eq!(s.name, "my_script");
    }

    #[tokio::test]
    async fn test_move_script() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/scripts/foo/move")
            .match_body(r#"{"target_project_id":9}"#)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(script_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client.project(1).scripts().move_to("foo", 9).await.unwrap();
        assert_eq!(s.id, 5);
    }

    #[tokio::test]
    async fn test_reorder_scripts() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PUT", "/projects/1/scripts/reorder")
            .match_body(r#"{"order":[5,4,3]}"#)
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        client
            .project(1)
            .scripts()
            .reorder(vec![5, 4, 3])
            .await
            .unwrap();
    }

    // ── drafts ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_draft_legacy_tuple_form() {
        // Pre-0.11.x servers (and simpler mocks) send inputs as 2-tuples.
        // Kept for forward-compat: the SDK must keep accepting this shape.
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/draft")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"source":"workflow main {}","inputs":[["doc","string"]]}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let draft = client
            .project(1)
            .drafts()
            .get("my_script")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(draft.source, "workflow main {}");
        assert_eq!(draft.inputs.len(), 1);
        assert_eq!(draft.inputs[0], ("doc".to_string(), "string".to_string()));
    }

    #[tokio::test]
    async fn test_get_draft_server_object_form() {
        // Regression test for issue #277: 0.11.x servers send inputs as
        // `[{name, ty: TypeRef, docs}]` and add a `type_defs` field. Large
        // scripts with non-empty inputs used to fail with
        // "error decoding response body" because the SDK's Draft model
        // expected the legacy `[["name","type"]]` tuple shape.
        let body = r#"{
            "source": "input doc: string",
            "inputs": [
                {
                    "name": "doc",
                    "ty": {"name": "string", "inner": null, "choices": null, "variants": null},
                    "docs": null
                },
                {
                    "name": "items",
                    "ty": {
                        "name": "list",
                        "inner": {"name": "string", "inner": null, "choices": null, "variants": null},
                        "choices": null,
                        "variants": null
                    },
                    "docs": "a list of strings"
                }
            ],
            "type_defs": {}
        }"#;
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/draft")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let draft = client
            .project(1)
            .drafts()
            .get("my_script")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(draft.inputs.len(), 2);
        assert_eq!(draft.inputs[0], ("doc".to_string(), "string".to_string()));
        assert_eq!(
            draft.inputs[1],
            ("items".to_string(), "list[string]".to_string())
        );
    }

    #[tokio::test]
    async fn test_get_draft_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/draft")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .drafts()
                .get("my_script")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_save_draft() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PUT", "/projects/1/scripts/my_script/draft")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"schema_warnings":[]}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let resp = client
            .project(1)
            .drafts()
            .save("my_script", "workflow main {}")
            .await
            .unwrap();
        assert!(resp.schema_warnings.is_empty());
    }

    // ── versions ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_versions() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", version_json());
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/versions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        let versions = client
            .project(1)
            .versions()
            .list("my_script")
            .await
            .unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].id, 3);
    }

    #[tokio::test]
    async fn test_get_version() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/versions/3")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(version_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let v = client
            .project(1)
            .versions()
            .get("my_script", 3)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(v.id, 3);
    }

    #[tokio::test]
    async fn test_get_version_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/versions/99")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .versions()
                .get("my_script", 99)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_get_latest_version() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":3,"script_id":5,"source":"workflow main {}","label":null,"published_by":null,"created_at":"2024-01-01T00:00:00Z","inputs":[]}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let v = client
            .project(1)
            .versions()
            .get_latest("my_script")
            .await
            .unwrap();
        assert!(v.is_some());
    }

    #[tokio::test]
    async fn test_publish_version() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/scripts/my_script/publish")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"version":{}}}"#, version_json()))
            .create_async()
            .await;

        let client = make_client(&server);
        let v = client
            .project(1)
            .versions()
            .publish_version("my_script", vec!["production".into()], Some("v1"), None)
            .await
            .unwrap();
        assert_eq!(v.id, 3);
    }

    // ── channels ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_channels() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", channel_json());
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/channels")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        let channels = client
            .project(1)
            .channels()
            .list("my_script")
            .await
            .unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "production");
    }

    #[tokio::test]
    async fn test_create_channel() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/scripts/my_script/channels")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(channel_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let ch = client
            .project(1)
            .channels()
            .create("my_script", "production")
            .await
            .unwrap();
        assert_eq!(ch.name, "production");
    }

    #[tokio::test]
    async fn test_delete_channel() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/1/scripts/my_script/channels/beta")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .channels()
                .delete("my_script", "beta")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_move_channel() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PATCH", "/projects/1/scripts/my_script/channels/staging")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .channels()
                .move_to("my_script", "staging", 3, false)
                .await
                .is_ok()
        );
    }

    // ── execution (builder) ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_builder() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock(
                "POST",
                "/projects/1/scripts/my_script/run?channel=production",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"execution_id":"abc-123"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let result = client
            .project(1)
            .executions()
            .run("my_script")
            .execute()
            .await
            .unwrap();
        assert_eq!(result.execution_id, "abc-123");
    }

    #[tokio::test]
    async fn test_run_404_surfaces_as_http_status() {
        // Regression test for issue #277: a 404 on POST paths used to be
        // silently passed through `send()` and then hit `res.json()` against
        // the error body, producing the generic
        // "error decoding response body" message. It must now surface as
        // HttpStatus{404, ...} with the server's error string readable.
        let mut server = Server::new_async().await;
        let _m = server
            .mock(
                "POST",
                "/projects/1/scripts/my_script/run?channel=nonexistent",
            )
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"channel 'nonexistent' not found"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client
            .project(1)
            .executions()
            .run("my_script")
            .channel("nonexistent")
            .execute()
            .await
            .unwrap_err();

        match err {
            AkribesError::HttpStatus { status, message } => {
                assert_eq!(status, 404);
                assert!(
                    message.contains("channel 'nonexistent' not found"),
                    "expected server body in message, got: {message}"
                );
            }
            other => panic!("expected HttpStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_decode_error_includes_body_snippet() {
        // Regression test for issue #277: when a response body doesn't
        // match the expected model, the error must include a snippet of
        // the actual body (not just "error decoding response body").
        let mut server = Server::new_async().await;
        let _m = server
            .mock(
                "POST",
                "/projects/1/scripts/my_script/run?channel=production",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            // Wrong shape — missing `execution_id`.
            .with_body(r#"{"surprise":"not what you wanted"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client
            .project(1)
            .executions()
            .run("my_script")
            .execute()
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("failed to decode")
                && msg.contains("RunResult")
                && msg.contains("surprise"),
            "decode error should name the target type and include body; got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_run_builder_typed_inputs_body() {
        let mut server = Server::new_async().await;
        // Asserts the wire format: multiple `.input(...)` calls plus the
        // `.document(...)` / `.documents(...)` shortcuts all merge into a
        // single `inputs` map.
        let _m = server
            .mock(
                "POST",
                "/projects/1/scripts/my_script/run?channel=production",
            )
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "inputs": {
                    "age": 25,
                    "name": "alice",
                    "resume": "doc_00000000-0000-0000-0000-000000000001",
                    "attachments": [
                        "doc_00000000-0000-0000-0000-000000000002",
                        "doc_00000000-0000-0000-0000-000000000003",
                    ],
                }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"execution_id":"abc-777"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let result = client
            .project(1)
            .executions()
            .run("my_script")
            .input("age", 25)
            .input("name", "alice")
            .document("resume", "doc_00000000-0000-0000-0000-000000000001")
            .documents(
                "attachments",
                [
                    "doc_00000000-0000-0000-0000-000000000002",
                    "doc_00000000-0000-0000-0000-000000000003",
                ],
            )
            .execute()
            .await
            .unwrap();
        assert_eq!(result.execution_id, "abc-777");
    }

    #[tokio::test]
    async fn test_run_builder_custom_channel() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/1/scripts/my_script/run?channel=staging")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"execution_id":"abc-456"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let result = client
            .project(1)
            .executions()
            .run("my_script")
            .channel("staging")
            .execute()
            .await
            .unwrap();
        assert_eq!(result.execution_id, "abc-456");
    }

    #[tokio::test]
    async fn test_run_and_await_builder() {
        let mut server = Server::new_async().await;
        let _run = server
            .mock(
                "POST",
                "/projects/1/scripts/my_script/run?channel=production",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"execution_id":"eid-1"}"#)
            .create_async()
            .await;
        let _out = server
            .mock("GET", "/executions/eid-1/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"completed","error":null,"error_kind":null,"result":null}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let (eid, out) = client
            .project(1)
            .executions()
            .run("my_script")
            .execute_and_await(None)
            .await
            .unwrap();
        assert_eq!(eid, "eid-1");
        assert_eq!(out.status, "completed");
    }

    // ── execution (direct) ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_cancel_run() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/1/scripts/my_script/run")
            .with_status(200)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .executions()
                .cancel_run("my_script")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_cancel_run_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/1/scripts/my_script/run")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            !client
                .project(1)
                .executions()
                .cancel_run("my_script")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_cancel_execution() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/executions/abc-123")
            .with_status(200)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(client.executions().cancel("abc-123").await.unwrap());
    }

    #[tokio::test]
    async fn test_get_execution() {
        let mut server = Server::new_async().await;
        let body = r#"{"id":"abc-123","project_id":1,"script_name":"my_script","status":"completed","started_at":null,"finished_at":null,"version_id":null,"channel":null,"error":null,"error_kind":null,"result":null,"documents":null,"triggered_by":null}"#;
        let _m = server
            .mock("GET", "/executions/abc-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let status = client.executions().get("abc-123").await.unwrap().unwrap();
        assert_eq!(status.id, "abc-123");
        assert_eq!(status.status, "completed");
    }

    #[tokio::test]
    async fn test_get_execution_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/executions/missing")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(client.executions().get("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_execution_output() {
        let mut server = Server::new_async().await;
        let body =
            r#"{"status":"completed","error":null,"error_kind":null,"result":{"answer":42}}"#;
        let _m = server
            .mock("GET", "/executions/abc-123/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let out = client
            .executions()
            .get_output("abc-123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.status, "completed");
        assert_eq!(out.result.unwrap()["answer"], 42);
    }

    #[tokio::test]
    async fn test_await_execution_immediate_completion() {
        let mut server = Server::new_async().await;
        let body = r#"{"status":"completed","error":null,"error_kind":null,"result":{"ok":true}}"#;
        let _m = server
            .mock("GET", "/executions/abc-123/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let out = client
            .executions()
            .await_execution("abc-123", None, Some(0))
            .await
            .unwrap();
        assert_eq!(out.status, "completed");
    }

    #[tokio::test]
    async fn test_await_execution_script_error() {
        let mut server = Server::new_async().await;
        let body = r#"{"status":"failed","error":"boom","error_kind":"ScriptError","result":null}"#;
        let _m = server
            .mock("GET", "/executions/abc-123/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client
            .executions()
            .await_execution("abc-123", None, Some(0))
            .await
            .unwrap_err();
        assert!(matches!(err, AkribesError::Script { .. }));
    }

    #[tokio::test]
    async fn test_await_execution_fatal_error() {
        let mut server = Server::new_async().await;
        let body =
            r#"{"status":"failed","error":"unauthorized","error_kind":"AuthError","result":null}"#;
        let _m = server
            .mock("GET", "/executions/abc-123/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client
            .executions()
            .await_execution("abc-123", None, Some(0))
            .await
            .unwrap_err();
        assert!(matches!(err, AkribesError::Fatal { .. }));
    }

    #[tokio::test]
    async fn test_await_execution_transient_error() {
        let mut server = Server::new_async().await;
        let body =
            r#"{"status":"failed","error":"rate limited","error_kind":"RateLimit","result":null}"#;
        let _m = server
            .mock("GET", "/executions/abc-123/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client
            .executions()
            .await_execution("abc-123", None, Some(0))
            .await
            .unwrap_err();
        assert!(matches!(err, AkribesError::Transient { .. }));
    }

    #[tokio::test]
    async fn test_await_execution_timeout() {
        let mut server = Server::new_async().await;
        let body = r#"{"status":"running","error":null,"error_kind":null,"result":null}"#;
        let _m = server
            .mock("GET", "/executions/timeout-id/output")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .expect_at_least(1)
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client
            .executions()
            .await_execution("timeout-id", Some(50), Some(10))
            .await
            .unwrap_err();
        assert!(matches!(err, AkribesError::Timeout { .. }));
    }

    // ── list executions (builder) ─────────────────────────────────────────────

    fn exec_status_json() -> &'static str {
        r#"{"id":"exec-1","project_id":1,"script_name":"my_script","status":"completed","started_at":"2026-04-02","finished_at":"2026-04-02","version_id":5,"channel":"production","error":null,"error_kind":null,"result":null,"documents":{"doc":"content"},"triggered_by":"studio"}"#
    }

    #[tokio::test]
    async fn test_list_executions() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", exec_status_json());
        let _m = server
            .mock("GET", "/projects/1/scripts/my_script/executions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        let execs = client
            .project(1)
            .executions()
            .list("my_script")
            .fetch()
            .await
            .unwrap();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].id, "exec-1");
        assert_eq!(execs[0].triggered_by.as_deref(), Some("studio"));
        assert!(execs[0].documents.is_some());
    }

    #[tokio::test]
    async fn test_list_executions_with_filters() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock(
                "GET",
                "/projects/1/scripts/my_script/executions?status=failed&channel=draft&limit=10&offset=5",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = make_client(&server);
        let execs = client
            .project(1)
            .executions()
            .list("my_script")
            .status("failed")
            .channel("draft")
            .limit(10)
            .offset(5)
            .fetch()
            .await
            .unwrap();
        assert!(execs.is_empty());
    }

    // ── execution events ──────────────────────────────────────────────────────

    fn mock_events_json() -> serde_json::Value {
        serde_json::json!([
            {"type": "WorkflowStart", "payload": 2},
            {"type": "TaskStart", "payload": ["summarise", null]},
            {"type": "AgentOutput", "payload": {
                "task_name": "summarise",
                "agent_name": null,
                "task_id": "t1",
                "schema_type": null,
                "chunk": "hello "
            }},
            {"type": "AgentOutput", "payload": {
                "task_name": "summarise",
                "agent_name": null,
                "task_id": "t1",
                "schema_type": null,
                "chunk": "world"
            }},
            // `WorkflowEnd(Value)` on the wire emits the clean form spec'd
            // in `docs/src/content/docs/reference/engine-events.mdx` —
            // a string output is the bare JSON string `"done"`, not the
            // engine's internal tagged-`Value` envelope `{"String":"done"}`.
            {"type": "WorkflowEnd", "payload": "done"},
        ])
    }

    #[tokio::test]
    async fn test_get_execution_events_completed() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({
            "execution_id": "exec-1",
            "status": "completed",
            "complete": true,
            "events": mock_events_json(),
        });
        let _m = server
            .mock("GET", "/executions/exec-1/events")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let client = make_client(&server);
        let result = client
            .executions()
            .get_events("exec-1", None, None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.execution_id, "exec-1");
        assert_eq!(result.status, "completed");
        assert!(result.complete);
        assert_eq!(result.events.len(), 5);
        assert!(matches!(
            result.events[0],
            crate::models::EngineEvent::WorkflowStart(2)
        ));
        match &result.events[2] {
            crate::models::EngineEvent::AgentOutput { chunk, .. } => {
                assert_eq!(chunk, "hello ");
            }
            other => panic!("expected AgentOutput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_execution_events_running_partial() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({
            "execution_id": "exec-2",
            "status": "running",
            "complete": false,
            "events": [
                {"type": "WorkflowStart", "payload": 2},
                {"type": "TaskStart", "payload": ["summarise", null]},
            ],
        });
        let _m = server
            .mock("GET", "/executions/exec-2/events")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let client = make_client(&server);
        let result = client
            .executions()
            .get_events("exec-2", None, None, None)
            .await
            .unwrap()
            .unwrap();
        assert!(!result.complete);
        assert_eq!(result.status, "running");
        assert_eq!(result.events.len(), 2);
    }

    #[tokio::test]
    async fn test_get_execution_events_empty() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({
            "execution_id": "exec-3",
            "status": "running",
            "complete": false,
            "events": [],
        });
        let _m = server
            .mock("GET", "/executions/exec-3/events")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let client = make_client(&server);
        let result = client
            .executions()
            .get_events("exec-3", None, None, None)
            .await
            .unwrap()
            .unwrap();
        assert!(result.events.is_empty());
    }

    #[tokio::test]
    async fn test_get_execution_events_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/executions/missing/events")
            .with_status(404)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .executions()
                .get_events("missing", None, None, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── clients & state ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_clients() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects/1/clients")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"id":"c1","name":"sdk","last_seen":"2024-01-01T00:00:00Z","scripts":["my-script"]}]"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let clients = client.project(1).registered_clients().list().await.unwrap();
        assert_eq!(clients.len(), 1);
    }

    #[tokio::test]
    async fn test_delete_client() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/clients/c1")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(
            client
                .project(1)
                .registered_clients()
                .delete("c1")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_get_state() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/state")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"env":{}}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let state = client.get_state().await.unwrap();
        assert!(state.get("env").is_some());
    }

    // ── tokens ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_tokens() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/tokens")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"id":"tok_1","label":"test","user_email":null,"scopes":{"projects":"*","role":"admin"},"minted_by":"studio","expires_at":"2026-02-01T00:00:00Z","revoked":false,"created_at":"2026-01-01T00:00:00Z","last_used_at":null}]"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let tokens = client.tokens().list().await.unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].label, "test");
    }

    #[tokio::test]
    async fn test_mint_token() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/tokens")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"aura_tk_abc123","token_id":"tok_1","expires_at":"2026-02-01T00:00:00Z"}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let req = MintTokenRequest {
            user_email: None,
            scopes: TokenScopes {
                projects: ProjectScope::Wildcard(WildcardMarker),
                role: TokenRole::Admin,
                scripts: None,
                executions: None,
                can_mint: false,
                features: vec![],
                org_id: None,
            },
            expires_in: 3600,
            label: "test".to_string(),
        };
        let res = client.tokens().mint(&req).await.unwrap();
        assert_eq!(res.token, "aura_tk_abc123");
        assert_eq!(res.token_id, "tok_1");
    }

    #[tokio::test]
    async fn test_revoke_token() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/tokens/tok_1")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        assert!(client.tokens().revoke("tok_1").await.is_ok());
    }

    // ── ad-hoc execution ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_sandbox_project_id() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/me/sandbox")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"project_id":42}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let pid = client.get_sandbox_project_id().await.unwrap();
        assert_eq!(pid, 42);
    }

    #[tokio::test]
    async fn test_run_adhoc() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/execute")
            .with_status(200)
            .with_header("content-type", "application/json")
            .match_body(r#"{"source":"workflow main {}"}"#)
            .with_body(r#"{"execution_id":"exec-1","project_id":42}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let res = client
            .run_adhoc("workflow main {}", None, None)
            .await
            .unwrap();
        assert_eq!(res.execution_id, "exec-1");
        assert_eq!(res.project_id, 42);
    }

    #[tokio::test]
    async fn test_run_adhoc_with_inputs() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/execute")
            .with_status(200)
            .with_header("content-type", "application/json")
            .match_body(r#"{"source":"workflow main {}","inputs":{"doc":"hello"}}"#)
            .with_body(r#"{"execution_id":"exec-2","project_id":42}"#)
            .create_async()
            .await;

        let client = make_client(&server);
        let mut inputs = std::collections::HashMap::new();
        inputs.insert(
            "doc".to_string(),
            serde_json::Value::String("hello".to_string()),
        );
        let res = client
            .run_adhoc("workflow main {}", Some(inputs), None)
            .await
            .unwrap();
        assert_eq!(res.execution_id, "exec-2");
    }

    // ── error classification ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fatal_error_on_401() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        assert!(matches!(err, AkribesError::Fatal { .. }));
    }

    #[tokio::test]
    async fn test_fatal_error_on_403() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(403)
            .with_body("Forbidden")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        assert!(matches!(err, AkribesError::Fatal { .. }));
    }

    #[tokio::test]
    async fn test_transient_error_on_429() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(429)
            .with_body("Too Many Requests")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        assert!(matches!(err, AkribesError::Transient { .. }));
    }

    #[tokio::test]
    async fn test_transient_error_on_503() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(503)
            .with_body("Service Unavailable")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        assert!(matches!(err, AkribesError::Transient { .. }));
    }

    /// Retry-After header is parsed into `Transient.retry_after` (#1009).
    #[tokio::test]
    async fn test_retry_after_populated_on_429() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(429)
            .with_header("Retry-After", "7")
            .with_body("Too Many Requests")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        match err {
            AkribesError::Transient {
                retry_after: Some(d),
                ..
            } => {
                assert_eq!(d.as_secs(), 7);
            }
            other => panic!("expected Transient with retry_after=Some(7s), got {other:?}"),
        }
    }

    /// Missing Retry-After yields `None` (not an error).
    #[tokio::test]
    async fn test_retry_after_none_when_header_absent() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(503)
            .with_body("down")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        match err {
            AkribesError::Transient {
                retry_after: None, ..
            } => {}
            other => panic!("expected Transient with retry_after=None, got {other:?}"),
        }
    }

    /// HTTP-date form is ignored (matches Python's behavior).
    #[tokio::test]
    async fn test_retry_after_ignored_on_http_date() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(429)
            .with_header("Retry-After", "Wed, 21 Oct 2026 07:28:00 GMT")
            .with_body("rate-limited")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        match err {
            AkribesError::Transient {
                retry_after: None, ..
            } => {}
            other => {
                panic!("expected Transient with retry_after=None for HTTP-date, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn test_transient_on_500_with_status() {
        // #1296: HTTP 500 routes to `AkribesError::Transient` with the
        // status code captured so callers can pick the right base backoff
        // via `AkribesError::recommended_backoff_ms(500)`. Previously this
        // fell through to the catch-all `HttpStatus` path.
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        assert!(
            matches!(
                err,
                AkribesError::Transient {
                    status: Some(500),
                    ..
                }
            ),
            "expected Transient with status=500, got {err:?}",
        );
    }

    #[tokio::test]
    async fn test_transient_on_504_with_status() {
        // #1296: HTTP 504 routes to `Transient` with status=504 captured.
        // `AkribesError::recommended_backoff_ms(504) > recommended_backoff_ms(500)`
        // expresses the per-status retry-policy split.
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/projects")
            .with_status(504)
            .with_body("Gateway Timeout")
            .create_async()
            .await;

        let client = make_client(&server);
        let err = client.projects().list().await.unwrap_err();
        assert!(
            matches!(
                err,
                AkribesError::Transient {
                    status: Some(504),
                    ..
                }
            ),
            "expected Transient with status=504, got {err:?}",
        );
        // Spot-check the per-status retry-policy table: 504 must back off
        // longer than 500/502 (slow upstream).
        let b504 = AkribesError::recommended_backoff_ms(504).unwrap();
        let b500 = AkribesError::recommended_backoff_ms(500).unwrap();
        let b502 = AkribesError::recommended_backoff_ms(502).unwrap();
        let b503 = AkribesError::recommended_backoff_ms(503).unwrap();
        assert!(b504 > b500);
        assert!(b504 > b502);
        // 503 mirrors 429.
        assert_eq!(b503, AkribesError::recommended_backoff_ms(429).unwrap());
        // 500 and 502 share the same short base.
        assert_eq!(b500, b502);
        assert_eq!(err.transient_status(), Some(504));
    }

    // ── missing project_id ───────────────────────────────────────────────────

    #[test]
    fn test_scoped_shim_without_project_id() {
        // A client built without project_id should fail the `.scoped()` shim
        // (used by consumers that pre-bind a project at construction time).
        // `.project(id)` remains infallible — it supplies the id directly.
        let client = AkribesClient::builder("http://localhost:3001")
            .name("test-app")
            .id("test-id")
            .build();

        assert!(matches!(
            client.scoped(),
            Err(AkribesError::MissingProjectId)
        ));
        // .project(id) is infallible and returns a ProjectScope directly.
        let _ = client.project(1).scripts();
    }

    // ── document helpers ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_document() {
        let mut server = Server::new_async().await;
        let body = r#"{"id":"doc_abc","filename":"report.pdf","content_type":"application/pdf","size_bytes":1024,"content_hash":"abc123","conversion_status":"ready","conversion_error":null,"created_at":"2026-04-12T00:00:00Z"}"#;
        let _m = server
            .mock("GET", "/documents/doc_abc")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = make_authed_client(&server);
        let doc = client.executions().get_document("doc_abc").await.unwrap();
        let doc = doc.expect("should return Some");
        assert_eq!(doc.id, "doc_abc");
        assert_eq!(doc.filename, "report.pdf");
        assert_eq!(doc.conversion_status, "ready");
    }

    #[tokio::test]
    async fn test_get_document_not_found() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/documents/doc_missing")
            .with_status(404)
            .create_async()
            .await;

        let client = make_authed_client(&server);
        let doc = client
            .executions()
            .get_document("doc_missing")
            .await
            .unwrap();
        assert!(doc.is_none());
    }

    #[tokio::test]
    async fn test_get_document_markdown() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/documents/doc_abc/markdown")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r##"{"markdown":"# Hello World"}"##)
            .create_async()
            .await;

        let client = make_authed_client(&server);
        let md = client
            .executions()
            .get_document_markdown("doc_abc")
            .await
            .unwrap();
        assert_eq!(md, "# Hello World");
    }

    #[tokio::test]
    async fn test_get_document_url() {
        let mut server = Server::new_async().await;
        let presigned = "https://s3.example.com/documents/doc_abc/report.pdf?token=xyz";
        let _m = server
            .mock("GET", "/documents/doc_abc/content")
            .with_status(200)
            .with_header("location", presigned)
            .create_async()
            .await;

        let client = make_authed_client(&server);
        let url = client
            .executions()
            .get_document_url("doc_abc")
            .await
            .unwrap();
        assert_eq!(url, presigned);
    }

    #[tokio::test]
    async fn test_reconvert_document() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/documents/doc_abc/convert")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ready"}"#)
            .create_async()
            .await;

        let client = make_authed_client(&server);
        let resp = client
            .executions()
            .reconvert_document("doc_abc")
            .await
            .unwrap();
        assert_eq!(resp["status"], "ready");
    }

    // ── Flat ProjectsClient cross-project script ops ──────────────────────────

    #[tokio::test]
    async fn test_projects_list_scripts_flat() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", script_json());
        let _m = server
            .mock("GET", "/projects/7/scripts")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        let scripts = client.projects().list_scripts(7).await.unwrap();
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].name, "my_script");
    }

    #[tokio::test]
    async fn test_projects_move_script_flat() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/3/scripts/foo/move")
            .match_body(r#"{"target_project_id":9}"#)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(script_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client.projects().move_script(3, "foo", 9).await.unwrap();
        assert_eq!(s.id, 5);
    }

    #[tokio::test]
    async fn test_projects_rename_script_flat() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PATCH", "/projects/3/scripts/old")
            .match_body(r#"{"new_name":"new"}"#)
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        client
            .projects()
            .rename_script(3, "old", "new")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_projects_delete_script_flat() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/3/scripts/foo")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        client.projects().delete_script(3, "foo").await.unwrap();
    }

    #[tokio::test]
    async fn test_projects_duplicate_script_flat() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/projects/3/scripts/foo/duplicate")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(script_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let s = client
            .projects()
            .duplicate_script(3, "foo", None)
            .await
            .unwrap();
        assert_eq!(s.name, "my_script");
    }

    // ── Flat lock helpers on RegisteredClientsClient ──────────────────────────

    fn lock_json() -> &'static str {
        r#"{
            "id":42,
            "client_id":"c1",
            "client_name":"sdk",
            "script_name":"my_script",
            "channel":"production",
            "bound_version_id":3,
            "lifetime":"persistent",
            "drifted":false,
            "created_by":null,
            "created_at":"2026-04-01T00:00:00Z",
            "input_schema":"{}"
        }"#
    }

    #[tokio::test]
    async fn test_list_locks_for_flat() {
        let mut server = Server::new_async().await;
        let body = format!("[{}]", lock_json());
        let _m = server
            .mock("GET", "/projects/7/scripts/my_script/locks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let client = make_client(&server);
        // The implicit project_id on the registered_clients client is `1`, but
        // `list_locks_for(7, ...)` must hit project 7 — that's the whole point
        // of the flat helper.
        let locks = client
            .project(1)
            .registered_clients()
            .list_locks_for(7, "my_script")
            .await
            .unwrap();
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].id, 42);
    }

    #[tokio::test]
    async fn test_delete_lock_flat() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("DELETE", "/projects/7/scripts/my_script/locks/42")
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server);
        client
            .project(1)
            .registered_clients()
            .delete_lock(7, "my_script", 42)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_update_lock_flat() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("PATCH", "/projects/7/scripts/my_script/locks/42/rebind")
            .match_body(r#"{"version_id":11}"#)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(lock_json())
            .create_async()
            .await;

        let client = make_client(&server);
        let lock = client
            .project(1)
            .registered_clients()
            .update_lock(7, "my_script", 42, Some(11))
            .await
            .unwrap();
        assert_eq!(lock.id, 42);
        assert_eq!(lock.bound_version_id, Some(3));
    }
}
