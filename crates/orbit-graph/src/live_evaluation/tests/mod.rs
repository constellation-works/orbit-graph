use super::*;

#[test]
fn precision_reserves_k_and_an_empty_cohort_is_null() {
    let spec = &VARIANTS[2];
    let mut accumulator = MetricAccumulator::default();
    accumulator.add(2, 1, 0.5, 10.0);
    accumulator.add(2, 0, 0.0, 30.0);
    let metric = accumulator.finish("all", spec, 10);
    assert_eq!(metric.cases, 2);
    assert_eq!(metric.relevant, 4);
    assert_eq!(metric.true_positives, 1);
    assert!((metric.recall_at_k.expect("recall") - 0.25).abs() < 1e-12);
    assert!((metric.precision_at_k.expect("precision") - 0.05).abs() < 1e-12);
    assert!((metric.mrr_at_k.expect("mrr") - 0.25).abs() < 1e-12);
    let empty = MetricAccumulator::default().finish("all", spec, 10);
    assert!(empty.recall_at_k.is_none());
    assert!(empty.precision_at_k.is_none());
    assert!(empty.mrr_at_k.is_none());
    assert!(subject_cites_task_id("fix: title [ORB-12] (#1)"));
    assert!(!subject_cites_task_id("fix: title without an id"));
}
