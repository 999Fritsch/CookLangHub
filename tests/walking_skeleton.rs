//! Acceptance tests for the walking skeleton.
//!
//! These tests assert observable product behavior: what a browser and an
//! orchestrator get. They do not assert how the application is arranged
//! internally, so a later change of the HTTP or Git internals keeps them
//! valid.

mod support;

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

#[tokio::test]
async fn health_reports_every_component_when_forgejo_answers() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    let response = reqwest::get(app.url("/health"))
        .await
        .expect("cannot reach the health endpoint");

    assert_eq!(response.status(), 200, "a healthy stack answers with 200");

    let body: Value = response.json().await.expect("the body is not JSON");

    assert_eq!(body["status"], "ok");
    assert_eq!(body["application"]["status"], "ok");
    assert_eq!(body["database"]["status"], "ok");
    assert_eq!(body["forgejo"]["status"], "ok");

    // The report names the Forgejo release, so an administrator can tell an
    // application fault from a Forgejo fault.
    let detail = body["forgejo"]["detail"]
        .as_str()
        .expect("the Forgejo component has no detail");
    assert!(
        detail.contains("Forgejo"),
        "the detail must name Forgejo, got `{detail}`"
    );
}

#[tokio::test]
async fn health_reports_a_forgejo_error_when_forgejo_is_absent() {
    let app = support::start_app(&support::unreachable_url().await).await;

    let response = reqwest::get(app.url("/health"))
        .await
        .expect("cannot reach the health endpoint");

    assert_eq!(
        response.status(),
        503,
        "an orchestrator must see the fault in the status code"
    );

    let body: Value = response.json().await.expect("the body is not JSON");

    assert_eq!(body["status"], "error");
    assert_eq!(body["forgejo"]["status"], "error");

    // A Forgejo outage must not hide the state of the other components.
    assert_eq!(body["application"]["status"], "ok");
    assert_eq!(body["database"]["status"], "ok");
}

#[tokio::test]
async fn the_index_page_is_server_rendered_html() {
    let app = support::start_app(&support::unreachable_url().await).await;

    let response = reqwest::get(app.url("/"))
        .await
        .expect("cannot reach the index page");

    assert_eq!(response.status(), 200);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .starts_with("text/html"),
        "the index page must be HTML"
    );

    let body = response.text().await.expect("cannot read the body");

    // Server-rendered: the markup arrives complete, without a client render
    // step.
    assert!(body.contains("<h1>CookLangHub</h1>"));
    assert!(body.trim_start().starts_with("<!DOCTYPE html>"));
}

#[tokio::test]
async fn no_page_asset_comes_from_another_host() {
    let app = support::start_app(&support::unreachable_url().await).await;

    let response = reqwest::get(app.url("/"))
        .await
        .expect("cannot reach the index page");

    let policy = response
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    // The policy stops the browser from loading an asset from a CDN.
    assert!(
        policy.contains("default-src 'self'"),
        "the page must restrict assets to this host, got `{policy}`"
    );

    let body = response.text().await.expect("cannot read the body");

    for attribute in ["src=\"http", "href=\"http", "@import url(http"] {
        // The Forgejo link is the one external address on the page, and it is
        // a link, not an asset. Assets must all be relative.
        let count = body.matches(attribute).count();
        let allowed = usize::from(attribute == "href=\"http");
        assert!(
            count <= allowed,
            "found {count} external `{attribute}` references on the index page"
        );
    }
}

#[tokio::test]
async fn the_stylesheet_comes_from_the_local_server() {
    let app = support::start_app(&support::unreachable_url().await).await;

    let response = reqwest::get(app.url("/static/app.css"))
        .await
        .expect("cannot reach the stylesheet");

    assert_eq!(response.status(), 200);

    let body = response.text().await.expect("cannot read the stylesheet");
    assert!(
        !body.contains("@import url(http"),
        "the stylesheet must not pull anything from another host"
    );
}

#[tokio::test]
async fn the_harness_deletes_the_forgejo_container_after_the_test() {
    let container_id = {
        let forgejo = support::start_forgejo().await;
        let id = forgejo.container_id();

        assert!(
            docker_container_exists(&id),
            "the container must exist while the test holds it"
        );

        id
    };

    // Dropping the harness removes the container. Removal is asynchronous on
    // the Docker side, so allow it a short time to finish.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && docker_container_exists(&container_id) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert!(
        !docker_container_exists(&container_id),
        "the harness must delete container {container_id} after the test"
    );
}

fn docker_container_exists(id: &str) -> bool {
    Command::new("docker")
        .args(["inspect", "--format", "{{.Id}}", id])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn a_page_links_to_forgejo_at_the_address_that_a_browser_can_reach() {
    // The application reaches Forgejo by its name on the internal network. A
    // browser cannot resolve that name, so every link must use the published
    // address instead.
    let internal = support::unreachable_url().await;
    let public = "https://forge.example.test";

    let app = support::start_app_with_public_forgejo_url(&internal, public).await;
    let body = reqwest::get(app.url("/"))
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        body.contains(public),
        "the page must link to the published Forgejo address"
    );
    assert!(
        !body.contains(&internal),
        "the page must not show the internal Forgejo address"
    );
}
