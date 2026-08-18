use std::cell::RefCell;

fn main() {
    let numbers = RefCell::new(vec![1, 2, 3]);
    numbers.borrow_mut().push(4);
    println!("runtime-checked value: {:?}", numbers.borrow());
}
