//! The host must be a Windows GUI program. As a console program, opening it
//! from the Start menu or the desktop also opens a black console window.

const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;
const IMAGE_SUBSYSTEM_WINDOWS_CUI: u16 = 3;

/// The Subsystem field of a PE image's optional header.
fn subsystem(image: &[u8]) -> Result<u16, String> {
    let read = |at: usize, len: usize| {
        image
            .get(at..at + len)
            .ok_or_else(|| format!("the image ends before byte {}", at + len))
    };
    let u16_at = |at| read(at, 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if read(0, 2)? != b"MZ" {
        return Err("no MZ header".into());
    }
    let pe = read(0x3c, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))? as usize;
    if read(pe, 4)? != b"PE\0\0" {
        return Err("no PE signature".into());
    }
    // The optional header follows the signature and the 20-byte file header.
    // Subsystem is at the same offset in PE32 and PE32+.
    let optional = pe + 24;
    match u16_at(optional)? {
        0x10b | 0x20b => u16_at(optional + 68),
        magic => Err(format!("unknown optional header magic {magic:#x}")),
    }
}

/// A PE header with nothing but the fields `subsystem` reads.
fn header(magic: u16, subsystem: u16) -> Vec<u8> {
    let optional = 0x80 + 24;
    let mut image = vec![0; optional + 70];
    image[..2].copy_from_slice(b"MZ");
    image[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    image[0x80..0x84].copy_from_slice(b"PE\0\0");
    image[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
    image[optional + 68..optional + 70].copy_from_slice(&subsystem.to_le_bytes());
    image
}

#[test]
fn reads_the_subsystem_of_32_and_64_bit_images() {
    for magic in [0x10b, 0x20b] {
        for kind in [IMAGE_SUBSYSTEM_WINDOWS_GUI, IMAGE_SUBSYSTEM_WINDOWS_CUI] {
            assert_eq!(subsystem(&header(magic, kind)), Ok(kind));
        }
    }
}

#[test]
fn rejects_what_is_not_a_pe_image() {
    assert!(subsystem(b"#!/bin/sh\n").is_err());
    assert!(subsystem(&header(0x20b, 2)[..0x90]).is_err());
    assert!(subsystem(&header(0x107, 2)).is_err());
}

#[cfg(windows)]
#[test]
fn the_host_opens_no_console_window() {
    let path = env!("CARGO_BIN_EXE_eternal-host");
    let image = std::fs::read(path).unwrap();
    assert_eq!(
        subsystem(&image),
        Ok(IMAGE_SUBSYSTEM_WINDOWS_GUI),
        "{path} must be a GUI program (2); a console program (3) opens a console window"
    );
}
