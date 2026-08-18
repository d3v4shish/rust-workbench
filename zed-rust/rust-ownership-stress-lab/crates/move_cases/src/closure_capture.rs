//! A `move` closure owns its captures even when invoked synchronously.

pub fn schedule_notification() {
    let subject = String::from("Build finished");
    let notify = move || println!("notification: {subject}");

    notify();
    println!("audit subject: {subject}");
}
