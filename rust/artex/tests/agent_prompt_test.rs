//! Differential tests for `agent/prompt.go:renderSystem` — port of
//! `TestRenderSystemOverrideAndFallback` + template conditionals.

use artex::agent::prompt::{render_system, render_template, Vars};

#[test]
fn render_system_override_and_fallback() {
    let vars = Vars::default().set("Goal", "g");
    assert_eq!(render_system("planner", "DEFAULT", &vars, None), "DEFAULT");

    let override_body =
        |k: &str| (k == "planner").then(|| "Goal:{{.Goal}} Scope:{{.Scope}}".to_string());
    let vars = Vars::default()
        .set("Goal", "capture X")
        .set("Scope", "*.x.com");
    assert_eq!(
        render_system("planner", "DEFAULT", &vars, Some(&override_body)),
        "Goal:capture X Scope:*.x.com"
    );

    let bad = |_k: &str| Some("{{.NotInCatalog}}".to_string());
    assert_eq!(
        render_system("planner", "DEFAULT", &vars, Some(&bad)),
        "DEFAULT"
    );
}

#[test]
fn template_conditionals() {
    let vars = Vars::default().set("ProxyAddr", "127.0.0.1:8080");
    let tmpl = "{{if .ProxyAddr}}via proxy {{.ProxyAddr}}{{else}}manual{{end}}";
    assert_eq!(
        render_template(tmpl, &vars).unwrap(),
        "via proxy 127.0.0.1:8080"
    );
    let empty = Vars::default();
    assert_eq!(render_template(tmpl, &empty).unwrap(), "manual");
    assert!(render_template("{{.Missing}}", &empty).is_err());
}
