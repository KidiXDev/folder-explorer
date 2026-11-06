use md5::Md5;
use sha1::Sha1;
use sha2::digest::DynDigest;
use sha2::{Sha256, Sha512};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

const BUFFER_SIZE: usize = 1 << 20;

fn hasher(slot: usize) -> Box<dyn DynDigest> {
    match slot {
        0 => Box::new(Md5::default()),
        1 => Box::new(Sha1::default()),
        2 => Box::new(Sha256::default()),
        _ => Box::new(Sha512::default()),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn hash_file(path: &Path, wanted: [bool; 4], cancel: &AtomicBool) -> io::Result<[String; 4]> {
    let mut file = File::open(path)?;
    let mut hashers: Vec<_> = (0..4).filter(|slot| wanted[*slot]).map(|slot| (slot, hasher(slot))).collect();
    let mut buffer = vec![0; BUFFER_SIZE];
    loop {
        if cancel.load(Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for (_, hasher) in &mut hashers {
            hasher.update(&buffer[..read]);
        }
    }
    let mut hashes: [String; 4] = Default::default();
    for (slot, hasher) in hashers {
        hashes[slot] = to_hex(&hasher.finalize());
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::TempTree;
    use std::fs;

    #[test]
    fn hashes_match_published_test_vectors() {
        let tree = TempTree::new();
        let path = tree.0.join("abc.txt");
        fs::write(&path, "abc").unwrap();
        let hashes = hash_file(&path, [true; 4], &AtomicBool::new(false)).unwrap();
        assert_eq!(hashes[0], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hashes[1], "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hashes[2], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(hashes[3].starts_with("ddaf35a193617abacc417349ae204131"));
    }

    #[test]
    fn unwanted_hashes_stay_empty() {
        let tree = TempTree::new();
        let path = tree.0.join("empty.txt");
        fs::write(&path, "").unwrap();
        let hashes = hash_file(&path, [false, false, true, false], &AtomicBool::new(false)).unwrap();
        assert!(hashes[0].is_empty() && hashes[1].is_empty() && hashes[3].is_empty());
        assert_eq!(hashes[2], "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn cancelling_stops_the_read() {
        let tree = TempTree::new();
        let path = tree.0.join("data.bin");
        fs::write(&path, vec![0; 16]).unwrap();
        let error = hash_file(&path, [true; 4], &AtomicBool::new(true)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }
}
