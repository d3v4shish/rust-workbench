//! Assignment destroys the old String while a view into it is still needed.

pub fn rotate_status() {
    let mut status = String::from("starting");
    let status_view = &status;

    status = String::from("ready");
    println!("transitioned from {status_view} to {status}");
}
