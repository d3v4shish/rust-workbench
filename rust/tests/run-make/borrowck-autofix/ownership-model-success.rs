use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    let values = Rc::new(RefCell::new(vec![1, 2, 3]));
    let shared = Rc::clone(&values);
    shared.borrow_mut().push(4);
    assert_eq!(values.borrow().len(), 4);
}
