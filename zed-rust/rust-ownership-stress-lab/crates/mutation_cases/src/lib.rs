//! Mutation failures used to compare plain `mut` with interior-mutability designs.

pub mod immutable_box;
pub mod immutable_vec;
pub mod rc_without_cell;
pub mod shared_reference;
