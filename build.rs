//! Gives velocity.exe its file icon: the logo from src/icon.rs, rasterised into a multi-size .ico and compiled
//! into the executable as a Windows resource (Explorer, the taskbar pin and shortcuts use it).

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

use std::path::PathBuf;

/// An .ico with one 32-bit BMP image per size (alpha carries the round edge; the AND mask is left empty).
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| {
            let rgba = icon::logo_rgba(s);
            let mask_row = (s as usize).div_ceil(32) * 4;
            let mut bmp = Vec::new();
            for v in [40u32, s, s * 2] {
                bmp.extend_from_slice(&v.to_le_bytes()); // header size, width, height (image + mask)
            }
            bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
            bmp.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
            bmp.extend_from_slice(&[0; 24]); // no compression, sizes and palette left to the reader
            for y in (0..s as usize).rev() {
                for x in 0..s as usize {
                    let p = &rgba[(y * s as usize + x) * 4..][..4];
                    bmp.extend_from_slice(&[p[2], p[1], p[0], p[3]]); // BGRA, bottom-up
                }
            }
            bmp.resize(bmp.len() + mask_row * s as usize, 0);
            bmp
        })
        .collect();
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let side = if s >= 256 { 0 } else { s as u8 }; // 0 means 256
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend(img);
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out.join("velocity.ico");
    std::fs::write(&ico_path, ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).unwrap();
    let rc = out.join("velocity.rc");
    let escaped = ico_path.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{escaped}\"\n")).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}
