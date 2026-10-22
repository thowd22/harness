use serde_json::json;

use super::*;

fn ctx() -> Value {
    let mut c = Context::default();
    c.inputs.insert("issue".into(), json!("#142 header bug"));
    c.inputs
        .insert("scope".into(), json!(["src/**", "tests/**"]));
    let mut repro = StepView {
        status: "succeeded".into(),
        ..StepView::default()
    };
    repro
        .outputs
        .insert("test_name".into(), json!("parse_header_crlf"));
    repro
        .artifacts
        .insert("plan".into(), "/runs/r1/artifacts/repro/plan.md".into());
    c.steps.insert("repro".into(), repro);
    c.matrix = Some(serde_json::Map::from_iter([(
        "os".to_owned(),
        json!("linux"),
    )]));
    c.item = Some(json!("a.rs"));
    c.run = RunView {
        id: "r1".into(),
        dir: "/runs/r1".into(),
    };
    c.to_value()
}

#[test]
fn renders_inputs_outputs_status_artifacts_matrix_item_and_run() {
    let out = render(
        "Fix {{ inputs.issue }} via {{ steps.repro.outputs.test_name }} ({{ steps.repro.status }}) \
         plan={{ steps.repro.artifacts.plan }} os={{ matrix.os }} item={{ item }} run={{ run.id }}@{{ run.dir }}",
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        out,
        "Fix #142 header bug via parse_header_crlf (succeeded) plan=/runs/r1/artifacts/repro/plan.md \
         os=linux item=a.rs run=r1@/runs/r1"
    );
    assert_eq!(render("plain text", &ctx()).unwrap(), "plain text");
}

#[test]
fn conditions_and_typed_values() {
    assert!(eval_bool("{{ steps.repro.status == 'succeeded' }}", &ctx()).unwrap());
    assert!(!eval_bool("steps.repro.outputs.test_name == 'x'", &ctx()).unwrap());
    assert_eq!(
        render_value(&json!("{{ inputs.scope }}"), &ctx()).unwrap(),
        json!(["src/**", "tests/**"])
    );
    assert_eq!(
        render_value(&json!({"a": "{{ run.id }}-x", "n": 3}), &ctx()).unwrap(),
        json!({"a": "r1-x", "n": 3})
    );
}

#[test]
fn no_environment_or_secret_access() {
    // Nothing outside the context exists, and undefined is an error.
    let err = render("{{ env.HOME }}", &ctx()).unwrap_err();
    assert!(err.message.contains("undefined"), "{}", err.message);
    assert!(render("{{ HOME }}", &ctx()).is_err());
    assert!(
        render("{{ debug() }}", &ctx()).is_err(),
        "debug() is removed"
    );
    let refs = references("{{ env.HOME }} {{ secrets.KEY }}").unwrap();
    let errs = check_refs(&refs, &RefScope::default());
    assert_eq!(errs.len(), 2);
    assert!(errs[0].contains("environment variables and secrets are not available"));
}

#[test]
fn references_are_checked_statically() {
    let inputs: BTreeSet<String> = ["issue".to_owned()].into();
    let steps: BTreeMap<String, StepRefs> = [
        (
            "repro".to_owned(),
            StepRefs {
                outputs: Some(["test_name".to_owned()].into()),
                artifacts: ["plan".to_owned()].into(),
            },
        ),
        ("later".to_owned(), StepRefs::default()),
    ]
    .into();
    let scope = RefScope {
        inputs: Some(&inputs),
        steps: Some(&steps),
        visible: ["repro".to_owned()].into(),
        matrix: None,
        item: false,
    };
    let ok = references("{{ inputs.issue }} {{ steps.repro.outputs.test_name }} {{ steps.repro.artifacts.plan }} {{ run.dir }} {% for i in range(3) %}{{ i }}{% endfor %}").unwrap();
    assert_eq!(check_refs(&ok, &scope), Vec::<String>::new());

    let bad = references(
        "{{ inputs.isue }} {{ steps.repro.outputs.tset }} {{ steps.nope.status }} {{ steps.later.status }} \
         {{ steps.repro.artifacts.x }} {{ steps.repro.foo }} {{ matrix.os }} {{ item }} {{ run.host }}",
    )
    .unwrap();
    let errs = check_refs(&bad, &scope).join("\n");
    for want in [
        "unknown input `isue`",
        "has no output `tset`",
        "unknown step `nope`",
        "`later` is not upstream",
        "has no artifact `x`",
        "unknown field `foo`",
        "has no `matrix`",
        "only defined in `for_each`",
        "use run.id or run.dir",
    ] {
        assert!(errs.contains(want), "missing {want:?} in\n{errs}");
    }
    assert!(references("{{ unclosed").is_err());
    assert!(expr_references("a ==").is_err());
}

#[test]
fn sole_expression_detection() {
    assert_eq!(sole_expression("{{ a.b }}"), Some("a.b"));
    assert_eq!(sole_expression("x {{ a }}"), None);
    assert_eq!(sole_expression("{{ a }} {{ b }}"), None);
}
