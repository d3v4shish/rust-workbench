//! A reference cannot survive transferring ownership of the referenced String.

fn queue(report: String) {
    println!("queued {report}");
}

pub fn queue_report() {
    let report = String::from("health-check.json");
    let extension = &report[report.len() - 4..];

    queue(report);
    println!("queued file extension: {extension}");
}
