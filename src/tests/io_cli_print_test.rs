use super::*;

#[test]
fn test_thousands() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(76_780_408), "76,780,408");
}

#[test]
fn test_cost_text() {
    let all_unpriced = TokenTotals {
        requests: 3,
        unpriced: 3,
        ..Default::default()
    };
    assert_eq!(cost_text(&all_unpriced), "-");
    let partial = TokenTotals {
        requests: 3,
        cost_usd: 1.5,
        unpriced: 1,
        ..Default::default()
    };
    assert_eq!(cost_text(&partial), "$1.50*");
    let small = TokenTotals {
        requests: 1,
        cost_usd: 0.0042,
        ..Default::default()
    };
    assert_eq!(cost_text(&small), "$0.0042");
}
