use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    let numbers = Rc::new(RefCell::new(vec![1, 2, 3]));
    let editor = Rc::clone(&numbers);
    editor.borrow_mut().push(4);
    println!("shared mutable value: {:?}", numbers.borrow());
}
