//! A normal ownership transfer into a function. Rc is not necessarily the right answer.

fn archive(document: String) {
    println!("archiving {document}");
}

pub fn publish_document() {
    let document = String::from("quarterly-report.pdf");
    archive(document);
    println!("published {document}");
}
