//! Differential tests for the minimal HTTP router — representative patterns
//! from `server/server.go:Handler`.

use artex::server::router::Router;

fn sample() -> Router {
    let mut r = Router::new();
    r.add("GET", "GET /api/tasks", "listTasks");
    r.add("POST", "POST /api/tasks", "createTask");
    r.add("GET", "GET /api/tasks/{id}", "getTask");
    r.add("PATCH", "PATCH /api/tasks/{id}", "updateTaskMetadata");
    r.add("POST", "POST /api/tasks/control/batch", "controlTasksBatch");
    r.add("GET", "GET /api/tasks/{id}/coverage", "taskCoverage");
    r.add("GET", "GET /api/health", "health");
    r
}

#[test]
fn router_matches_methods_and_params() {
    let r = sample();
    assert_eq!(r.len(), 7);

    let m = r.match_route("GET", "/api/tasks/7").unwrap();
    assert_eq!(m.route.handler, "getTask");
    assert_eq!(m.params.get("id").map(String::as_str), Some("7"));

    let m = r.match_route("PATCH", "/api/tasks/7").unwrap();
    assert_eq!(m.route.handler, "updateTaskMetadata");

    assert_eq!(
        r.match_route("GET", "/api/tasks").unwrap().route.handler,
        "listTasks"
    );
    assert_eq!(
        r.match_route("POST", "/api/tasks").unwrap().route.handler,
        "createTask"
    );
    assert_eq!(
        r.match_route("GET", "/api/health").unwrap().route.handler,
        "health"
    );
}

#[test]
fn router_prefers_literal_over_wildcard() {
    let r = sample();
    let m = r.match_route("POST", "/api/tasks/control/batch").unwrap();
    assert_eq!(
        m.route.handler, "controlTasksBatch",
        "literal route must beat GET /api/tasks/{{id}}"
    );
    let m = r.match_route("GET", "/api/tasks/7/coverage").unwrap();
    assert_eq!(m.route.handler, "taskCoverage");
    assert_eq!(m.params.get("id").map(String::as_str), Some("7"));
}

#[test]
fn router_404_and_405() {
    let r = sample();
    assert!(r.match_route("GET", "/api/nope").is_none());
    assert!(r.match_route("DELETE", "/api/tasks/7").is_none());
    assert!(r.method_not_allowed("DELETE", "/api/tasks/7"));
    assert!(!r.method_not_allowed("GET", "/api/nope"));
    assert!(Router::new().is_empty());
}
