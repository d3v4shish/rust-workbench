pub struct Record {
    pub key: String,
    pub value: String,
}

pub fn move_and_restore() -> Record {
    let mut record = Record {
        key: "id".into(),
        value: "42".into(),
    };
    let old_key = record.key;
    record.key = old_key.to_uppercase();
    println!("restored record: {}={}", record.key, record.value);
    record
}
