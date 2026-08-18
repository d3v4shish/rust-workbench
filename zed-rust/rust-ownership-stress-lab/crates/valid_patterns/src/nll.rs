pub fn non_lexical_lifetime() -> String {
    let mut message = String::from("hello");
    let view = &message;
    println!("last shared use: {view}");
    message.push('!');
    message
}
