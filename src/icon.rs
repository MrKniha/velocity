//! The Velocity logo, rasterised at runtime for the window/taskbar icon so no image files ship.
//! Same shapes as assets/logo.svg: a white V (one rounded stroke) on a near-black circle.

const GROUND: [f32; 3] = [22.0, 23.0, 26.0]; // #16171a
const V: [(f32, f32); 3] = [(18.7, 20.7), (28.5, 43.7), (45.3, 19.9)];
const STROKE: f32 = 6.8;

/// Distance from p to the segment a-b.
fn segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// RGBA pixels of the logo at `size` x `size`.
pub fn logo_rgba(size: u32) -> Vec<u8> {
    let unit = 64.0 / size as f32; // design units per pixel
    let cover = |d: f32| (0.5 - d / unit).clamp(0.0, 1.0); // d: signed distance in design units
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = ((x as f32 + 0.5) * unit, (y as f32 + 0.5) * unit);
            let circle = cover(((p.0 - 32.0).powi(2) + (p.1 - 32.0).powi(2)).sqrt() - 32.0);
            let v = cover(segment(p, V[0], V[1]).min(segment(p, V[1], V[2])) - STROKE / 2.0);
            let [r, g, b] = GROUND.map(|c| (c + (255.0 - c) * v) as u8);
            out.extend_from_slice(&[r, g, b, (circle * 255.0) as u8]);
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
        assert_eq!(at(0, 0)[3], 0, "outside the circle is transparent");
        assert_eq!(at(28, 43)[..3], [255, 255, 255], "the V's point is white");
        assert_eq!(at(45, 20)[..3], [255, 255, 255], "the long arm's tip is white");
        assert_eq!(at(32, 56)[..], [22, 23, 26, 255], "the rest of the circle is near-black");
    }
}
