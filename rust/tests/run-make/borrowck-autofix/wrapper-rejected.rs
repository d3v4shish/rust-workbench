fn consume(_: Box<Vec<i32>>) {}

fn main() {
    let values = Box::new(vec![1]);
    consume(values);
    if values.len() != 1 {
        std::process::exit(1);
    }
}
