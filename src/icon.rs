//! The Velocity logo, rasterised at runtime for the window/taskbar icon so no image files ship.
//! Same shapes as assets/logo.svg: a white dot with two fading trails on a near-black tile.

const GROUND: f32 = 20.0; // #141416 (grey; the blue channel is +2, close enough at icon size)

/// Signed distance to a rounded rectangle centred at (cx, cy) with half-size (hx, hy) and radius r.
fn rounded_rect(p: (f32, f32), c: (f32, f32), h: (f32, f32), r: f32) -> f32 {
    let qx = (p.0 - c.0).abs() - (h.0 - r);
    let qy = (p.1 - c.1).abs() - (h.1 - r);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// RGBA pixels of the logo at `size` x `size`.
pub fn logo_rgba(size: u32) -> Vec<u8> {
    let unit = 64.0 / size as f32; // design units per pixel
    let cover = |d: f32| (0.5 - d / unit).clamp(0.0, 1.0);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = ((x as f32 + 0.5) * unit, (y as f32 + 0.5) * unit);
            let trail = cover(rounded_rect(p, (23.0, 32.0), (13.0, 3.0), 3.0)) * 0.35;
            let short = cover(rounded_rect(p, (25.0, 22.0), (7.0, 2.0), 2.0)) * 0.2;
            let dot = cover(((p.0 - 42.0).powi(2) + (p.1 - 32.0).powi(2)).sqrt() - 10.0);
            let white = dot.max(trail).max(short);
            let v = (GROUND + (255.0 - GROUND) * white) as u8;
            out.extend_from_slice(&[v, v, v + (2.0 * (1.0 - white)) as u8, (cover(rounded_rect(p, (32.0, 32.0), (32.0, 32.0), 14.0)) * 255.0) as u8]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_shape() {
        let px = logo_rgba(64);
        assert_eq!(px.len(), 64 * 64 * 4);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "corner outside the tile is transparent");
        assert_eq!(at(42, 32)[..3], [255, 255, 255], "dot is white");
        let trail = at(20, 32)[0];
        assert!(trail > 80 && trail < 120, "trail is ~35% white, got {trail}");
        assert_eq!(at(56, 50)[0], 20, "empty tile is ground");
    }
}
