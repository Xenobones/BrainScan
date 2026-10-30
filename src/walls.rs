//! Wall model, material attenuation, and floor-plan wall detection.
//!
//! Detection is deliberately simple and predictable: it finds long straight horizontal and
//! vertical dark strokes, which is how nearly all architectural plans draw walls. Text,
//! furniture and door swings produce only short runs and are ignored. Diagonal or curved
//! walls are not detected; draw those by hand.

use serde::{Deserialize, Serialize};

use crate::model::Band;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Material {
    Drywall,
    Wood,
    Glass,
    Brick,
    CinderBlock,
    Concrete,
    ReinforcedConcrete,
    Metal,
}

impl Material {
    pub const ALL: [Material; 8] = [
        Material::Drywall,
        Material::Wood,
        Material::Glass,
        Material::Brick,
        Material::CinderBlock,
        Material::Concrete,
        Material::ReinforcedConcrete,
        Material::Metal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Material::Drywall => "Drywall",
            Material::Wood => "Wood / door",
            Material::Glass => "Glass",
            Material::Brick => "Brick",
            Material::CinderBlock => "Cinder block",
            Material::Concrete => "Concrete",
            Material::ReinforcedConcrete => "Reinforced concrete",
            Material::Metal => "Metal / elevator",
        }
    }

    /// Typical single-wall attenuation in dB. Rough rule-of-thumb figures; real walls vary
    /// with thickness, rebar, coatings and angle of incidence.
    pub fn loss_db(self, band: Band) -> f64 {
        let (g24, g5) = match self {
            Material::Drywall => (3.0, 4.0),
            Material::Wood => (4.0, 5.0),
            Material::Glass => (3.0, 4.0),
            Material::Brick => (6.0, 9.0),
            Material::CinderBlock => (8.0, 12.0),
            Material::Concrete => (12.0, 18.0),
            Material::ReinforcedConcrete => (16.0, 24.0),
            Material::Metal => (20.0, 30.0),
        };
        match band {
            Band::Ghz2_4 => g24,
            Band::Ghz5 => g5,
            Band::Ghz6 => g5 * 1.1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wall {
    pub id: u32,
    /// Endpoints in floor-plan pixels.
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub thickness_px: f64,
    #[serde(default)]
    pub material: Option<Material>,
}

impl Wall {
    pub fn length_px(&self) -> f64 {
        ((self.x2 - self.x1).powi(2) + (self.y2 - self.y1).powi(2)).sqrt()
    }

    pub fn is_horizontal(&self) -> bool {
        (self.x2 - self.x1).abs() >= (self.y2 - self.y1).abs()
    }

    pub fn distance_to(&self, p: (f64, f64)) -> f64 {
        point_segment_distance(p, (self.x1, self.y1), (self.x2, self.y2))
    }

    /// Multiplies every coordinate by `f` (used to map from a downscaled detection image).
    pub fn scaled(&self, f: f64) -> Wall {
        Wall {
            x1: self.x1 * f,
            y1: self.y1 * f,
            x2: self.x2 * f,
            y2: self.y2 * f,
            thickness_px: self.thickness_px * f,
            ..self.clone()
        }
    }
}

pub fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    ((p.0 - (a.0 + t * dx)).powi(2) + (p.1 - (a.1 + t * dy)).powi(2)).sqrt()
}

fn orient(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// True if segments p1-p2 and p3-p4 properly cross.
pub fn segments_cross(p1: (f64, f64), p2: (f64, f64), p3: (f64, f64), p4: (f64, f64)) -> bool {
    let (d1, d2) = (orient(p3, p4, p1), orient(p3, p4, p2));
    let (d3, d4) = (orient(p1, p2, p3), orient(p1, p2, p4));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) && d1 != 0.0 && d2 != 0.0 && d3 != 0.0 && d4 != 0.0
}

/// Total attenuation of every material-assigned wall the straight path a -> b crosses.
/// `a`, `b` and the walls must share a coordinate system.
pub fn path_loss_db(walls: &[Wall], a: (f64, f64), b: (f64, f64), band: Band) -> f64 {
    walls
        .iter()
        .filter(|w| segments_cross(a, b, (w.x1, w.y1), (w.x2, w.y2)))
        .filter_map(|w| w.material.map(|m| m.loss_db(band)))
        .sum()
}

// ------------------------------------------------------------------- detection

#[derive(Debug, Clone, PartialEq)]
pub struct DetectOptions {
    /// Shortest stroke (pixels) that counts as a wall.
    pub min_len_px: usize,
    /// Thickest stroke that counts; bigger dark areas are fills, not walls.
    pub max_thickness_px: f64,
    /// Minimum length / thickness.
    pub min_aspect: f64,
    pub max_walls: usize,
}

impl DetectOptions {
    pub fn for_size(width: usize, height: usize) -> DetectOptions {
        let m = width.max(height) as f64;
        DetectOptions {
            min_len_px: (m * 0.04).max(16.0) as usize,
            max_thickness_px: (m * 0.02).max(6.0),
            min_aspect: 4.0,
            max_walls: 1500,
        }
    }
}

/// Otsu threshold; returns a "dark" mask where dark means ink on a light page (inverted
/// automatically for dark-background plans like blueprints).
fn binarize(luma: &[u8]) -> Vec<bool> {
    let (lo, hi) = luma.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    if hi.saturating_sub(lo) < 40 {
        return vec![false; luma.len()];
    }
    let mut hist = [0u64; 256];
    for &v in luma {
        hist[v as usize] += 1;
    }
    let total = luma.len() as f64;
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let (mut w0, mut sum0, mut best, mut thr) = (0.0, 0.0, -1.0, 127u8);
    for t in 0..256usize {
        w0 += hist[t] as f64;
        if w0 == 0.0 {
            continue;
        }
        let w1 = total - w0;
        if w1 == 0.0 {
            break;
        }
        sum0 += t as f64 * hist[t] as f64;
        let (m0, m1) = (sum0 / w0, (sum_all - sum0) / w1);
        let between = w0 * w1 * (m0 - m1).powi(2);
        if between > best {
            best = between;
            thr = t as u8;
        }
    }
    let dark: Vec<bool> = luma.iter().map(|&v| v <= thr).collect();
    let dark_count = dark.iter().filter(|d| **d).count();
    if dark_count * 2 > luma.len() { dark.iter().map(|d| !d).collect() } else { dark }
}

/// Marks pixels that lie inside a dark run of at least `min_len` along the given axis.
fn long_runs(dark: &[bool], w: usize, h: usize, min_len: usize, horizontal: bool) -> Vec<bool> {
    let mut mask = vec![false; dark.len()];
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { o * w + i } else { i * w + o };
    for o in 0..outer {
        let mut start: Option<usize> = None;
        for i in 0..=inner {
            let on = i < inner && dark[idx(o, i)];
            match (on, start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    if i - s >= min_len {
                        for k in s..i {
                            mask[idx(o, k)] = true;
                        }
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    mask
}

fn components(mut mask: Vec<bool>, w: usize, h: usize, horizontal: bool, o: &DetectOptions, out: &mut Vec<Wall>) {
    let mut stack = Vec::new();
    for sy in 0..h {
        for sx in 0..w {
            if !mask[sy * w + sx] {
                continue;
            }
            mask[sy * w + sx] = false;
            stack.push((sx, sy));
            let (mut minx, mut maxx, mut miny, mut maxy, mut count) = (sx, sx, sy, sy, 0usize);
            while let Some((x, y)) = stack.pop() {
                count += 1;
                minx = minx.min(x);
                maxx = maxx.max(x);
                miny = miny.min(y);
                maxy = maxy.max(y);
                let mut push = |nx: usize, ny: usize| {
                    if mask[ny * w + nx] {
                        mask[ny * w + nx] = false;
                        stack.push((nx, ny));
                    }
                };
                if x > 0 { push(x - 1, y); }
                if y > 0 { push(x, y - 1); }
                if x + 1 < w { push(x + 1, y); }
                if y + 1 < h { push(x, y + 1); }
            }
            let (len, span_other) = if horizontal { (maxx - minx + 1, maxy - miny + 1) } else { (maxy - miny + 1, maxx - minx + 1) };
            let thickness = count as f64 / len as f64;
            if len < o.min_len_px || thickness > o.max_thickness_px || len as f64 / thickness < o.min_aspect || span_other as f64 > o.max_thickness_px * 2.0 {
                continue;
            }
            let (x1, y1, x2, y2) = if horizontal {
                let cy = (miny + maxy + 1) as f64 / 2.0;
                (minx as f64, cy, (maxx + 1) as f64, cy)
            } else {
                let cx = (minx + maxx + 1) as f64 / 2.0;
                (cx, miny as f64, cx, (maxy + 1) as f64)
            };
            out.push(Wall { id: 0, x1, y1, x2, y2, thickness_px: thickness.max(1.0), material: None });
        }
    }
}

/// Detects axis-aligned walls in a grayscale image (`luma.len() == width * height`).
/// Coordinates are in the same pixel space as the buffer. Longest walls first, ids from 1.
pub fn detect_walls(luma: &[u8], width: usize, height: usize, opts: &DetectOptions) -> Vec<Wall> {
    if luma.len() != width * height || width == 0 || height == 0 {
        return Vec::new();
    }
    let dark = binarize(luma);
    let mut walls = Vec::new();
    for horizontal in [true, false] {
        components(long_runs(&dark, width, height, opts.min_len_px, horizontal), width, height, horizontal, opts, &mut walls);
    }
    walls.sort_by(|a, b| b.length_px().total_cmp(&a.length_px()));
    walls.truncate(opts.max_walls);
    for (i, w) in walls.iter_mut().enumerate() {
        w.id = i as u32 + 1;
    }
    walls
}

// -------------------------------------------------------------------- grouping

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WallGroup {
    Exterior,
    Interior,
    InteriorThick,
    InteriorThin,
}

impl WallGroup {
    pub fn label(self) -> &'static str {
        match self {
            WallGroup::Exterior => "Exterior walls",
            WallGroup::Interior => "Interior walls",
            WallGroup::InteriorThick => "Thick interior walls",
            WallGroup::InteriorThin => "Thin interior walls",
        }
    }

    /// A sensible starting guess; the user confirms or changes it.
    pub fn suggested_material(self) -> Material {
        match self {
            WallGroup::Exterior => Material::Brick,
            WallGroup::InteriorThick => Material::CinderBlock,
            WallGroup::Interior | WallGroup::InteriorThin => Material::Drywall,
        }
    }
}

/// Splits walls into exterior (hugging the outline of all walls) and interior, with interior
/// split into thick/thin when thickness clearly varies. Returns `(group, wall ids)`.
pub fn group_walls(walls: &[Wall]) -> Vec<(WallGroup, Vec<u32>)> {
    if walls.is_empty() {
        return Vec::new();
    }
    let xs = walls.iter().flat_map(|w| [w.x1, w.x2]);
    let ys = walls.iter().flat_map(|w| [w.y1, w.y2]);
    let (minx, maxx) = xs.fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
    let (miny, maxy) = ys.fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
    let (mx, my) = ((maxx - minx) * 0.03, (maxy - miny) * 0.03);

    let is_exterior = |w: &Wall| {
        if w.is_horizontal() {
            (w.y1 - miny).abs() <= my || (maxy - w.y1).abs() <= my
        } else {
            (w.x1 - minx).abs() <= mx || (maxx - w.x1).abs() <= mx
        }
    };
    let (ext, inner): (Vec<&Wall>, Vec<&Wall>) = walls.iter().partition(|w| is_exterior(w));

    let mut groups: Vec<(WallGroup, Vec<u32>)> = Vec::new();
    if !ext.is_empty() {
        groups.push((WallGroup::Exterior, ext.iter().map(|w| w.id).collect()));
    }
    if !inner.is_empty() {
        let (tmin, tmax) = inner.iter().fold((f64::MAX, 0.0f64), |(a, b), w| (a.min(w.thickness_px), b.max(w.thickness_px)));
        if tmax >= tmin * 1.5 {
            let split = (tmin * tmax).sqrt();
            let (thick, thin): (Vec<&Wall>, Vec<&Wall>) = inner.iter().partition(|w| w.thickness_px >= split);
            groups.push((WallGroup::InteriorThick, thick.iter().map(|w| w.id).collect()));
            groups.push((WallGroup::InteriorThin, thin.iter().map(|w| w.id).collect()));
        } else {
            groups.push((WallGroup::Interior, inner.iter().map(|w| w.id).collect()));
        }
    }
    groups.retain(|(_, ids)| !ids.is_empty());
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Canvas {
        w: usize,
        h: usize,
        px: Vec<u8>,
    }

    impl Canvas {
        fn new(w: usize, h: usize) -> Canvas {
            Canvas { w, h, px: vec![255; w * h] }
        }
        fn rect(&mut self, x: usize, y: usize, rw: usize, rh: usize, v: u8) {
            for yy in y..(y + rh).min(self.h) {
                for xx in x..(x + rw).min(self.w) {
                    self.px[yy * self.w + xx] = v;
                }
            }
        }
        fn detect(&self) -> Vec<Wall> {
            detect_walls(&self.px, self.w, self.h, &DetectOptions::for_size(self.w, self.h))
        }
    }

    /// 400x300 building: 6 px outer walls, one 3 px interior wall with a door gap, noise.
    fn building() -> Canvas {
        let mut c = Canvas::new(400, 300);
        c.rect(20, 20, 360, 6, 0); // top
        c.rect(20, 274, 360, 6, 0); // bottom
        c.rect(20, 20, 6, 260, 0); // left
        c.rect(374, 20, 6, 260, 0); // right
        c.rect(23, 150, 120, 3, 0); // interior, left of door
        c.rect(183, 150, 194, 3, 0); // interior, right of door (40 px gap)
        // "text" and furniture: short strokes and a small filled box
        for i in 0..6 {
            c.rect(200 + i * 8, 60, 4, 10, 0);
        }
        c.rect(250, 200, 18, 18, 0);
        c
    }

    #[test]
    fn finds_outer_walls_and_interior_pieces_but_not_text_or_furniture() {
        let walls = building().detect();
        assert_eq!(walls.len(), 6, "{walls:#?}");
        let horizontals = walls.iter().filter(|w| w.is_horizontal()).count();
        assert_eq!(horizontals, 4); // top, bottom, two interior pieces
        let top = walls.iter().find(|w| w.is_horizontal() && (w.y1 - 23.0).abs() < 1.0).unwrap();
        assert!((top.length_px() - 360.0).abs() <= 1.0);
        assert!((top.thickness_px - 6.0).abs() < 0.5);
        let ids: Vec<u32> = walls.iter().map(|w| w.id).collect();
        assert_eq!(ids, (1..=6).collect::<Vec<_>>());
        assert!(walls[0].length_px() >= walls[5].length_px());
    }

    #[test]
    fn door_gap_splits_the_interior_wall() {
        let walls = building().detect();
        let mut interior: Vec<&Wall> = walls.iter().filter(|w| w.is_horizontal() && (w.y1 - 151.5).abs() < 1.0).collect();
        interior.sort_by(|a, b| a.x1.total_cmp(&b.x1));
        assert_eq!(interior.len(), 2);
        assert!(interior[0].x2 < 150.0 && interior[1].x1 > 180.0);
    }

    #[test]
    fn dark_background_plans_are_inverted_automatically() {
        let mut c = building();
        for v in &mut c.px {
            *v = 255 - *v;
        }
        assert_eq!(c.detect().len(), 6);
    }

    #[test]
    fn blank_or_mismatched_input_yields_nothing() {
        assert!(Canvas::new(100, 100).detect().is_empty());
        assert!(detect_walls(&[0; 10], 5, 5, &DetectOptions::for_size(5, 5)).is_empty());
        assert!(detect_walls(&[], 0, 0, &DetectOptions::for_size(0, 0)).is_empty());
    }

    #[test]
    fn grouping_finds_exterior_and_splits_thick_from_thin() {
        let mut c = building();
        c.rect(300, 100, 8, 120, 0); // thick interior vertical wall
        let walls = c.detect();
        let groups = group_walls(&walls);
        let get = |g: WallGroup| groups.iter().find(|(k, _)| *k == g).map(|(_, v)| v.len());
        assert_eq!(get(WallGroup::Exterior), Some(4));
        assert_eq!(get(WallGroup::InteriorThick), Some(1));
        assert_eq!(get(WallGroup::InteriorThin), Some(2));
        assert_eq!(group_walls(&[]).len(), 0);
        // uniform interior thickness stays one group
        let g2 = group_walls(&building().detect());
        assert!(g2.iter().any(|(k, v)| *k == WallGroup::Interior && v.len() == 2));
    }

    #[test]
    fn segment_crossing_and_path_loss() {
        assert!(segments_cross((0.0, 0.0), (10.0, 10.0), (0.0, 10.0), (10.0, 0.0)));
        assert!(!segments_cross((0.0, 0.0), (1.0, 1.0), (5.0, 0.0), (5.0, 9.0)));
        let w = |id, x: f64, m| Wall { id, x1: x, y1: -50.0, x2: x, y2: 50.0, thickness_px: 3.0, material: m };
        let walls = [w(1, 5.0, Some(Material::Concrete)), w(2, 8.0, Some(Material::Drywall)), w(3, 9.0, None)];
        let loss = path_loss_db(&walls, (0.0, 0.0), (20.0, 0.0), Band::Ghz5);
        assert_eq!(loss, 18.0 + 4.0); // unassigned wall adds nothing
        assert_eq!(path_loss_db(&walls, (0.0, 0.0), (4.0, 0.0), Band::Ghz5), 0.0);
        assert!(Material::Concrete.loss_db(Band::Ghz5) > Material::Concrete.loss_db(Band::Ghz2_4));
    }

    #[test]
    fn distance_to_segment() {
        let w = Wall { id: 1, x1: 0.0, y1: 0.0, x2: 10.0, y2: 0.0, thickness_px: 2.0, material: None };
        assert_eq!(w.distance_to((5.0, 3.0)), 3.0);
        assert_eq!(w.distance_to((13.0, 4.0)), 5.0);
        let s = w.scaled(2.0);
        assert_eq!((s.x2, s.thickness_px), (20.0, 4.0));
    }
}
