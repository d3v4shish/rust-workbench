use std::cell::RefCell;

pub fn runtime_checked_mutation() -> Vec<String> {
    let log = RefCell::new(Vec::new());
    log.borrow_mut().push(String::from("started"));
    log.borrow_mut().push(String::from("finished"));
    log.into_inner()
}
