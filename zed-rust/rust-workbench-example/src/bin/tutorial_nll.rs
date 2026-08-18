fn main() {
    let mut message = String::from("hello");
    let view = &message;

    println!("borrowed: {view}");
    message.push('!');
    println!("after the borrow ended: {message}");
}
