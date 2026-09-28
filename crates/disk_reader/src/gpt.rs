//! Minimal read-only GPT parser — just enough to find the `backingfiles`
//! partition on a SentryUSB SSD. Hand-rolled instead of pulling in the
//! `gpt` crate: we need only the header sanity check and the entry array.

use std::io::{Read, Seek, SeekFrom};

use anyhow::{bail, Context, Result};

const SECTOR: u64 = 512;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";

#[derive(Clone, Debug)]
pub struct GptPartition {
    pub index: u32,
    pub name: String,
    /// Byte offset of the partition start on the disk.
    pub start: u64,
    /// Length in bytes.
    pub len: u64,
}

pub fn read_partitions<R: Read + Seek>(dev: &mut R) -> Result<Vec<GptPartition>> {
    let mut header = [0u8; 92];
    dev.seek(SeekFrom::Start(SECTOR))
        .context("seek to GPT header")?;
    dev.read_exact(&mut header).context("read GPT header")?;
    if &header[0..8] != GPT_SIGNATURE {
        bail!("no GPT signature");
    }

    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let num_entries = u32::from_le_bytes(header[80..84].try_into().unwrap());
    let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
    if entry_size < 128 || num_entries > 512 {
        bail!("implausible GPT entry layout ({num_entries} x {entry_size})");
    }

    let mut table = vec![0u8; entry_size * num_entries as usize];
    dev.seek(SeekFrom::Start(entries_lba * SECTOR))
        .context("seek to GPT entries")?;
    dev.read_exact(&mut table).context("read GPT entries")?;

    let mut parts = Vec::new();
    for i in 0..num_entries as usize {
        let e = &table[i * entry_size..(i + 1) * entry_size];
        let type_guid: [u8; 16] = e[0..16].try_into().unwrap();
        if type_guid == [0u8; 16] {
            continue; // unused slot
        }
        let first_lba = u64::from_le_bytes(e[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(e[40..48].try_into().unwrap());
        let name: String = e[56..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .map(|u| char::from_u32(u as u32).unwrap_or('?'))
            .collect();
        parts.push(GptPartition {
            index: i as u32 + 1,
            name,
            start: first_lba * SECTOR,
            len: (last_lba + 1 - first_lba) * SECTOR,
        });
    }
    Ok(parts)
}
