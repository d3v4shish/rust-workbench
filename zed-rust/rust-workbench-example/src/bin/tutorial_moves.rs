fn main() {
    let message = String::from("hello");
    let owner = message;

    println!("new owner: {owner}");
    println!("old owner: {message}");
}
