use std::cmp::Ordering;

pub mod belf;
pub mod permission;
pub mod xkrn;

pub use belf::*;
pub use permission::*;
pub use xkrn::*;

pub(crate) const PAGE_SIZE: usize = 4096;

pub fn align_size_up(offset: usize, alignment_offset: usize) -> usize {
    match (offset % PAGE_SIZE).cmp(&alignment_offset) {
        Ordering::Equal => offset,
        Ordering::Less => offset + (alignment_offset - offset % PAGE_SIZE),
        Ordering::Greater => (offset & !(PAGE_SIZE - 1)) + PAGE_SIZE + alignment_offset,
    }
}

pub fn align_data_up(data: &[u8], alignment_offset: usize) -> Vec<u8> {
    if data.len() % PAGE_SIZE == alignment_offset {
        data.to_vec()
    } else {
        let padding = align_size_up(data.len(), alignment_offset) - data.len();
        let pad = vec![0u8; padding];
        [data, &pad[..]].concat().to_vec()
    }
}
