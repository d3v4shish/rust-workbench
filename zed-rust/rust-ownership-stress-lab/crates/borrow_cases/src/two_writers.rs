//! Two live mutable views of the same vector violate Rust's one-writer rule.

pub fn update_ledger() {
    let mut ledger = vec![100, 200, 300];
    let debit_writer = &mut ledger;
    let credit_writer = &mut ledger;

    debit_writer.push(-25);
    credit_writer.push(25);
}
