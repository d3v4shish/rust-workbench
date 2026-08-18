use std::rc::Rc;

fn main() {
    let values: Rc<Vec<i32>> = Rc::new(Vec::new());

    values.push(1);
    println!("items: {}", values.len());
}
