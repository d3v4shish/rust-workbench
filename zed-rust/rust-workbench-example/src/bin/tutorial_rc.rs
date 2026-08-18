use std::rc::Rc;

fn main() {
    let numbers = Rc::new(vec![1, 2, 3]);
    let first_view = Rc::clone(&numbers);
    let second_view = Rc::clone(&numbers);

    println!("{first_view:?} {second_view:?}");
}
