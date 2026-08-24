fn main() {
    let boxed = Box::new(String::from("owned"));
    let moved = boxed;
    assert_eq!(&*moved, "owned");
}
