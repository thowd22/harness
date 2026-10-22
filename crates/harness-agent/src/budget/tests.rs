use super::*;
use harness_config::ini::config::Config;

#[test]
fn budgets_come_from_the_budget_section() {
    let mut cfg = Config::defaults();
    cfg.budget.session_usd = Some(10.0);
    cfg.budget.run_usd = Some(5.0);
    cfg.budget.daily_usd = None;
    cfg.budget.on_exceed = OnExceed::Stop;
    let b = Budgets::from_config(&cfg.budget)
        .with_limit(BudgetScope::Agent, Some(1.0))
        .with_limit(BudgetScope::Workflow, Some(2.0));
    assert_eq!(b.limit(BudgetScope::Session), Some(10.0));
    assert_eq!(b.limit(BudgetScope::Run), Some(5.0));
    assert_eq!(b.limit(BudgetScope::Day), None);
    assert_eq!(b.limit(BudgetScope::Agent), Some(1.0));
    assert_eq!(b.limit(BudgetScope::Workflow), Some(2.0));
    assert_eq!(b.on_exceed, OnExceed::Stop);
    assert_eq!(
        b.with_limit(BudgetScope::Run, None).limit(BudgetScope::Run),
        None
    );
}

#[test]
fn meter_warns_once_at_80_percent_and_reports_exceeded_scopes() {
    let meter = BudgetMeter::new(
        Budgets::new()
            .with_limit(BudgetScope::Session, Some(10.0))
            .with_limit(BudgetScope::Day, Some(30.0)),
    )
    .with_spent(BudgetScope::Day, 15.0);
    meter.charge(7.0);
    assert_eq!(
        meter.check(),
        BudgetCheck {
            warnings: vec![],
            exceeded: None
        }
    );
    meter.charge(1.0);
    let c = meter.check();
    assert_eq!(c.warnings.len(), 1);
    assert_eq!(c.warnings[0].scope, BudgetScope::Session);
    assert_eq!(
        c.warnings[0].to_string(),
        "budget warning: session spend $8.00 is 80% of the $10.00 budget"
    );
    assert!(meter.check().warnings.is_empty(), "warned once");
    meter.charge(2.5);
    let c = meter.check();
    let ex = c.exceeded.unwrap();
    assert_eq!(ex.scope, BudgetScope::Session);
    assert_eq!(
        ex.to_string(),
        "session budget of $10.00 reached ($10.5000 spent)"
    );
    assert_eq!(meter.spent(BudgetScope::Day), 25.5);
    // An approval raises the limit by its original amount.
    meter.extend(BudgetScope::Session);
    assert_eq!(meter.limit(BudgetScope::Session), Some(20.0));
    assert_eq!(meter.check().exceeded, None);
}

#[test]
fn run_scope_restarts_per_run() {
    let meter = BudgetMeter::new(Budgets::new().with_limit(BudgetScope::Run, Some(1.0)));
    meter.charge(1.0);
    assert_eq!(meter.check().exceeded.unwrap().scope, BudgetScope::Run);
    meter.start_run();
    assert_eq!(meter.spent(BudgetScope::Run), 0.0);
    assert_eq!(meter.spent(BudgetScope::Session), 1.0);
    assert_eq!(meter.check().exceeded, None);
}
