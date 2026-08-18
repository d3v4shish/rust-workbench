//! The value moves on only one control-flow path, so it is unavailable after the branch.

fn transmit(payload: Vec<u8>) {
    println!("sent {} bytes", payload.len());
}

pub fn maybe_transmit(online: bool) {
    let payload = vec![0xde, 0xad, 0xbe, 0xef];
    if online {
        transmit(payload);
    }
    println!("retaining {} bytes for retry", payload.len());
}
