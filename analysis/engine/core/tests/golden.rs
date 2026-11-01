//! Holds latent-core to tools/latent_golden.py's dumps:
//!   uv run --script analysis/tools/latent_golden.py
//!   cargo test -p latent-core -- --nocapture
//! Without analysis/engine/golden these tests pass with a note, so a checkout without the
//! raws and packs still builds.
use latent_core::develop::{gamut_compress, shaper, tetrahedral, Frame, Tables};
use latent_core::firmware::Firmware;
use latent_core::grain::{fnv1a_seed, grain_noise, Grain};
use latent_core::halation::{self, Halation, Offsets, HALO_LEVELS};
use latent_core::lite::{self, Film, GRAIN_BLOCK, GRID};
use latent_core::meta::{open_raw, RawInfo, Source};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join("..")
}

fn lite_dir() -> PathBuf {
    root().join("analysis/app/public/LATENT_LITE")
}

fn cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../golden");
    let Ok(list) = fs::read_dir(dir) else {
        eprintln!("no golden data, skipped");
        return vec![];
    };
    let mut out: Vec<_> = list.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("rgb.bin").exists()).collect();
    out.sort();
    out
}

fn f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn f64s(b: &[u8]) -> Vec<f64> {
    b.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect()
}

fn u16s(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes(c.try_into().unwrap())).collect()
}

fn i32s(b: &[u8]) -> Vec<i32> {
    b.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().flat_map(|x| if x.is_array() { floats(x) } else { vec![x.as_f64().unwrap()] }).collect()
}

fn uints(v: &Value) -> Vec<u32> {
    floats(v).iter().map(|x| *x as u32).collect()
}

fn firmware() -> Firmware {
    Firmware::new(fs::read(lite_dir().join("firmware.bin")).unwrap())
}

fn film(code: &str) -> Film {
    serde_json::from_slice(&fs::read(lite_dir().join(code).join("film.json")).unwrap()).unwrap()
}

fn raw_info(m: &Value) -> RawInfo {
    let r = &m["raw"];
    let p = uints(&r["pattern"]);
    let low = p[0] | p[1] << 2 | p[2] << 4 | p[3] << 6;
    let u4 = |k: &str| -> [u32; 4] { uints(&r[k]).try_into().unwrap() };
    let f4 = |k: &str| -> [f32; 4] {
        let v: Vec<f32> = floats(&r[k]).iter().map(|x| *x as f32).collect();
        v.try_into().unwrap()
    };
    let xyz = floats(&r["cam_xyz"]);
    let size = uints(&r["size"]);
    RawInfo {
        colors: r["colors"].as_i64().unwrap() as i32,
        filters: low * 0x01010101,
        black_level_per_channel: u4("black"),
        linear_max: u4("linear_max"),
        maximum: r["maximum"].as_u64().unwrap() as u32,
        cam_mul: f4("cam_mul"),
        pre_mul: f4("pre_mul"),
        cam_xyz: [0, 3, 6].map(|i| [xyz[i] as f32, xyz[i + 1] as f32, xyz[i + 2] as f32]),
        width: size[0],
        height: size[1],
        top_margin: size[2],
        left_margin: size[3],
        crop: u4("crop"),
        // Golden raws are CR2/ARW: IFD0 always wins over LibRaw's make/model/flip.
        make: String::new(),
        model: String::new(),
        flip: 0,
    }
}

/// Largest absolute difference and how many values differ at all.
fn diff<T: Copy + Into<f64>>(a: &[T], b: &[T]) -> (f64, usize) {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).fold((0.0, 0), |(mx, n), (x, y)| {
        let d: f64 = ((*x).into() - (*y).into()).abs();
        (mx.max(d), n + (d != 0.0) as usize)
    })
}

struct Case {
    dir: PathBuf,
    name: String,
    m: Value,
    fw: Firmware,
    film: Film,
    src: Source,
}

impl Case {
    fn open(dir: PathBuf) -> Case {
        let m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let fw = firmware();
        let film = film(m["case"]["film"].as_str().unwrap());
        let data = fs::read(root().join(m["case"]["file"].as_str().unwrap())).unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let src = open_raw(&data, &fw, &raw_info(&m)).unwrap_or_else(|e| panic!("{name}: {e} ({:?})", raw_info(&m)));
        Case { dir, name, m, fw, film, src }
    }

    fn file(&self, name: &str) -> Vec<u8> {
        fs::read(self.dir.join(name)).unwrap_or_else(|_| panic!("{}: no {name}", self.name))
    }

    fn has(&self, name: &str) -> bool {
        self.dir.join(name).exists()
    }

    fn matrix(&self) -> latent_core::color::M3 {
        let ev = self.m["case"]["exposure"].as_f64().unwrap();
        let scale = ev.exp2();
        self.src.matrix.map(|r| r.map(|v| v * scale))
    }

    fn tables(&self) -> Vec<u16> {
        lite::tables(&fs::read(lite_dir().join(&self.film.code).join("tables.bin")).unwrap()).unwrap()
    }

    fn selection(&self) -> lite::Selection {
        let c = &self.m["case"];
        let mired = c["kelvin"].as_f64().map_or(self.src.mired, |k| 1e6 / k);
        lite::select_cubes(&self.film, c["paper"].as_u64().unwrap() as usize, c["push"].as_i64().unwrap() as i32, c["grade"].as_u64().unwrap() as usize, mired).unwrap()
    }

    /// The cube, rebuilt the way Studio.cube does.
    fn cube(&self, sel: &lite::Selection) -> Vec<f32> {
        let (n_push, n_light) = (self.film.push.len(), self.film.light_mired.len());
        let paper = sel.i0 / (n_push * n_light);
        let p = &self.film.papers[paper];
        let blob = fs::read(lite_dir().join(&self.film.code).join(p.file.as_ref().unwrap())).unwrap();
        let basis = lite::decode_basis(&blob, p.delta, p.push_w[0].len() * p.light_w[0].len()).unwrap();
        let curve = lite::output_curve(&self.tables());
        let c0 = lite::cube16(&self.film, &basis, &curve, sel.i0).unwrap();
        if sel.i1 != sel.i0 && sel.weight > 0.0 {
            lite::blend(&c0, &lite::cube16(&self.film, &basis, &curve, sel.i1).unwrap(), sel.weight)
        } else {
            c0
        }
    }

    fn grain(&self) -> Option<(Grain, Vec<i16>)> {
        let c = &self.m["case"];
        if !c["grain"].as_bool().unwrap() {
            return None;
        }
        let paper = c["paper"].as_u64().unwrap();
        let push_i = self.film.push.iter().position(|p| *p as i64 == c["push"].as_i64().unwrap()).unwrap();
        let blob = fs::read(lite_dir().join(&self.film.code).join(format!("grain{paper}.bin"))).unwrap();
        let blocks = lite::decode_grain(&blob, self.film.push.len()).unwrap();
        let g = Grain::new(&blocks[push_i * GRAIN_BLOCK..][..GRAIN_BLOCK], c["grade"].as_u64().unwrap() as usize).unwrap();
        let expected = &self.m["grain"];
        assert_eq!(g.simple, expected["simple"].as_bool().unwrap(), "{}", self.name);
        assert_eq!(g.mono, expected["mono"].as_bool().unwrap(), "{}", self.name);
        let path = self.src.card_dir;
        let file = c["file"].as_str().unwrap().to_uppercase();
        let card = format!("A:/DCIM/{path}/{file}");
        assert_eq!(card, self.m["card_path"].as_str().unwrap());
        let seed = fnv1a_seed(&card);
        assert_eq!(seed as u64, self.m["seed"].as_u64().unwrap(), "{}", self.name);
        let (h, w) = (self.height(), self.width());
        let noise = grain_noise(&self.fw, seed, h, w, g.streams()).unwrap();
        Some((g, noise))
    }

    fn width(&self) -> usize { self.m["width"].as_u64().unwrap() as usize }
    fn height(&self) -> usize { self.m["height"].as_u64().unwrap() as usize }

    fn offsets(&self, rgb: &[u16]) -> Option<Offsets> {
        let level = self.m["halation"].as_u64()? as u32;
        assert!(HALO_LEVELS.contains(&level));
        let profile = halation::profile(&self.fw, &self.film.code).unwrap().unwrap();
        let h = Halation::new(&self.fw, &profile, &self.matrix(), self.src.clip).unwrap();
        let maps = h.build_map(rgb, self.width(), self.height());
        if self.has("maps.bin") {
            let (d, _) = diff(&maps, &f64s(&self.file("maps.bin")));
            println!("{:14} maps diff {d:e}", self.name);
            assert!(d < 1e-7, "{}: maps differ by {d}", self.name);
        }
        Some(Offsets::new(&maps, &profile, level, self.height(), self.width()))
    }
}

#[test]
fn source_matches_open_raw() {
    for dir in cases() {
        let c = Case::open(dir);
        let (m, s) = (&c.m, &c.src);
        assert_eq!(s.model, m["model"].as_str().unwrap(), "{}", c.name);
        assert_eq!(s.make, m["make"].as_str().unwrap(), "{}", c.name);
        assert_eq!(s.orientation as u64, m["orientation"].as_u64().unwrap(), "{}", c.name);
        assert_eq!(s.card_dir, m["card_dir"].as_str().unwrap(), "{}", c.name);
        assert_eq!(s.black as i64, m["black"].as_i64().unwrap(), "{}", c.name);
        assert_eq!(s.white as i64, m["white"].as_i64().unwrap(), "{}", c.name);
        assert_eq!(s.crop.map(|v| v as u32).to_vec(), uints(&m["crop_used"]), "{}", c.name);
        assert_eq!(s.wb.to_vec(), floats(&m["wb"]), "{}", c.name);
        let py = floats(&m["matrix"]);
        for (a, b) in s.matrix.iter().flatten().zip(&py) {
            assert!((a - b).abs() < 1e-9, "{}: matrix {a} vs {b}", c.name);
        }
        for (a, b) in s.clip.iter().zip(floats(&m["clip"])) {
            assert_eq!(*a as f64, b, "{}: clip", c.name);
        }
        let mired = m["scene_mired"].as_f64().unwrap();
        assert!((s.mired - mired).abs() < 1e-9, "{}: mired {} vs {mired}", c.name, s.mired);
        let sel = c.selection();
        assert_eq!(vec![sel.i0 as u32, sel.i1 as u32], uints(&m["cubes"]), "{}", c.name);
        assert!((sel.weight - m["weight"].as_f64().unwrap()).abs() < 1e-12, "{}", c.name);
        assert_eq!(sel.curves.is_some(), m["bw_curves"].as_bool().unwrap(), "{}", c.name);
    }
}

#[test]
fn noise_is_exact() {
    for dir in cases() {
        let c = Case::open(dir);
        if let Some((_, noise)) = c.grain() {
            let py = i32s(&c.file("noise.bin"));
            assert_eq!(noise.len(), py.len(), "{}", c.name);
            assert!(noise.iter().zip(&py).all(|(a, b)| *a as i32 == *b), "{}: noise differs", c.name);
            println!("{:14} noise exact, {} values", c.name, noise.len());
        }
    }
}

#[test]
fn cubes_match() {
    for dir in cases() {
        let c = Case::open(dir);
        if !c.has("cube.bin") {
            continue;
        }
        // Python sums the bases in float32 through BLAS, in an order of its own. Where the
        // output table is flat (clipped blacks and whites) from_levels magnifies that rounding,
        // so the 16-bit values can move there while the 8-bit output cannot.
        let (mine, py) = (c.cube(&c.selection()), f32s(&c.file("cube.bin")));
        let words = c.tables();
        let t = Tables::new(&words);
        let level = |v: &f32| t.out8[((*v as u32 * 0xFFF + 0x8000) >> 16) as usize] as i32;
        let worst = mine.iter().zip(&py).map(|(a, b)| (level(a) - level(b)).abs()).max().unwrap();
        let far = mine.iter().zip(&py).filter(|(a, b)| (*a - *b).abs() > 1.0).count();
        let (d, n) = diff(&mine, &py);
        println!("{:14} cube max diff {d:.3}, {n} differ, {far} by over 1, 8-bit diff {worst}", c.name);
        assert!(worst <= 1, "{}: cube differs by {worst} output levels", c.name);
    }
}

#[test]
fn stages_match() {
    for dir in cases() {
        let c = Case::open(dir);
        if !c.has("g.bin") {
            continue;
        }
        let f8 = c.m["lin_dtype"].as_str().unwrap() == "f8";
        let words = c.tables();
        let t = Tables::new(&words);
        let rgb = u16s(&c.file("rgb.bin"));
        let matrix = c.matrix().map(|r| r.map(|v| v as f32));
        let cube = f32s(&c.file("cube.bin"));
        let py_g = f64s(&c.file("g.bin"));
        let py_tetra = f32s(&c.file("tetra.bin"));
        let gamut_on = c.m["case"]["gamut"].as_bool().unwrap();
        let curves: [&[u8]; 3] = [&t.out8, &t.out8, &t.out8];
        let frame = Frame {
            rgb: &rgb, width: c.width(), height: c.height(), clip: c.src.clip, matrix, tables: &t, out8: curves,
            cube: &cube, gamut: gamut_on, halo: None, grain: None,
        };
        if !f8 {
            let py = f32s(&c.file("lin_in.bin"));
            let lin: Vec<f32> = rgb.chunks_exact(3).flat_map(|p| frame.linear(p)).collect();
            let (d, n) = diff(&lin, &py);
            println!("{:14} linear max diff {d:e}, {n} differ", c.name);
            assert!(d < 1e-3, "{}: linear differs by {d}", c.name);
            let py_out = f32s(&c.file("lin.bin"));
            let mut g = Vec::with_capacity(py.len());
            let mut out = Vec::with_capacity(py.len());
            for p in py.chunks_exact(3) {
                let l = [p[0], p[1], p[2]];
                let o = if gamut_on { gamut_compress(l, &t.gamut) } else { l };
                out.extend(o);
                g.extend(shaper(o, &t.shaper));
            }
            let (d, n) = diff(&out, &py_out);
            println!("{:14} gamut max diff {d:e}, {n} differ", c.name);
            assert!(d == 0.0, "{}: gamut differs by {d}", c.name);
            let (d, n) = diff(&g, &py_g);
            println!("{:14} shaper max diff {d:e}, {n} differ", c.name);
            assert!(d == 0.0, "{}: shaper differs by {d}", c.name);
        } else {
            let py = f64s(&c.file("lin_in.bin"));
            let py_out = f64s(&c.file("lin.bin"));
            let mut g = Vec::with_capacity(py.len());
            let mut out = Vec::with_capacity(py.len());
            for p in py.chunks_exact(3) {
                let l = [p[0], p[1], p[2]];
                let o = if gamut_on { gamut_compress(l, &t.gamut) } else { l };
                out.extend(o);
                g.extend(shaper(o, &t.shaper));
            }
            let (d, n) = diff(&out, &py_out);
            println!("{:14} gamut64 max diff {d:e}, {n} differ", c.name);
            assert!(d == 0.0, "{}: gamut differs by {d}", c.name);
            let (d, n) = diff(&g, &py_g);
            println!("{:14} shaper64 max diff {d:e}, {n} differ", c.name);
            assert!(d == 0.0, "{}: shaper differs by {d}", c.name);
        }
        let tetra: Vec<f32> = py_g.chunks_exact(3)
            .flat_map(|g| tetrahedral::<3>(&cube, GRID, [g[0], g[1], g[2]]).map(|v| v.clamp(0.0, 65535.0)))
            .collect();
        let (d, n) = diff(&tetra, &py_tetra);
        println!("{:14} tetra max diff {d:e}, {n} differ", c.name);
        assert!(d == 0.0, "{}: tetrahedral differs by {d}", c.name);
    }
}

#[test]
fn ramp_edges() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../golden/ramp");
    if !dir.exists() {
        return;
    }
    let read = |n: &str| fs::read(dir.join(n)).unwrap();
    let words = lite::tables(&fs::read(lite_dir().join("P400/tables.bin")).unwrap()).unwrap();
    let t = Tables::new(&words);
    let (lin, py, py_g) = (f32s(&read("lin_in.bin")), f32s(&read("lin.bin")), f64s(&read("g.bin")));
    let (py64, py_g64) = (f64s(&read("lin64.bin")), f64s(&read("g64.bin")));
    let (mut o32, mut g32, mut o64, mut g64) = (vec![], vec![], vec![], vec![]);
    for p in lin.chunks_exact(3) {
        let l = [p[0], p[1], p[2]];
        let o = gamut_compress(l, &t.gamut);
        o32.extend(o);
        g32.extend(shaper(o, &t.shaper));
        let o = gamut_compress(l.map(|v| v as f64), &t.gamut);
        o64.extend(o);
        g64.extend(shaper(o, &t.shaper));
    }
    assert_eq!(diff(&o32, &py), (0.0, 0));
    assert_eq!(diff(&g32, &py_g), (0.0, 0));
    assert_eq!(diff(&o64, &py64), (0.0, 0));
    assert_eq!(diff(&g64, &py_g64), (0.0, 0));
}

/// From Python's own linear values on, the pipeline has to land on the same bytes: all that
/// separates the two is the float32 rounding of `cam @ m.T`, which BLAS does with FMA.
#[test]
fn output_exact_from_python_linear() {
    for dir in cases() {
        let c = Case::open(dir);
        if !c.has("lin_in.bin") {
            continue;
        }
        let words = c.tables();
        let t = Tables::new(&words);
        let rgb = u16s(&c.file("rgb.bin"));
        let sel = c.selection();
        let cube = f32s(&c.file("cube.bin"));
        let curves_blob = sel.curves.map(|_| lite::inflate(&fs::read(lite_dir().join(&c.film.code).join("curves.bin")).unwrap()).unwrap());
        let out8: [&[u8]; 3] = match (&curves_blob, sel.curves) {
            (Some(b), Some(base)) => [0, 1, 2].map(|i| &b[base + 0x1000 * i..base + 0x1000 * (i + 1)]),
            _ => [&t.out8, &t.out8, &t.out8],
        };
        let grain = c.grain();
        let gamut_on = c.m["case"]["gamut"].as_bool().unwrap();
        let frame = Frame {
            rgb: &rgb, width: c.width(), height: c.height(), clip: c.src.clip,
            matrix: c.matrix().map(|r| r.map(|v| v as f32)), tables: &t, out8, cube: &cube,
            gamut: gamut_on, halo: None, grain: grain.as_ref().map(|(g, n)| (g, n.as_slice())),
        };
        let (w, n) = (c.width(), c.width() * c.height());
        let mut out = vec![0u8; n * 3];
        let bytes = c.file("lin_in.bin");
        if c.m["lin_dtype"].as_str().unwrap() == "f8" {
            let lin = f64s(&bytes);
            for i in 0..n {
                let l = [lin[i * 3], lin[i * 3 + 1], lin[i * 3 + 2]];
                let g = shaper(if gamut_on { gamut_compress(l, &t.gamut) } else { l }, &t.shaper);
                frame.finish(g, i / w, i % w, &mut out[i * 3..i * 3 + 3]);
            }
        } else {
            let lin = f32s(&bytes);
            for i in 0..n {
                let l = [lin[i * 3], lin[i * 3 + 1], lin[i * 3 + 2]];
                let g = shaper(if gamut_on { gamut_compress(l, &t.gamut) } else { l }, &t.shaper);
                frame.finish(g, i / w, i % w, &mut out[i * 3..i * 3 + 3]);
            }
        }
        let (d, differ) = diff(&out, &c.file("out.bin"));
        println!("{:14} from Python linear: max diff {d}, {differ} differ", c.name);
        assert_eq!(differ, 0, "{}", c.name);
    }
}

#[test]
fn output_within_one_level() {
    for dir in cases() {
        let c = Case::open(dir);
        let words = c.tables();
        let t = Tables::new(&words);
        let rgb = u16s(&c.file("rgb.bin"));
        let sel = c.selection();
        let cube = c.cube(&sel);
        let curves_blob = sel.curves.map(|_| lite::inflate(&fs::read(lite_dir().join(&c.film.code).join("curves.bin")).unwrap()).unwrap());
        let out8: [&[u8]; 3] = match (&curves_blob, sel.curves) {
            (Some(b), Some(base)) => [0, 1, 2].map(|i| &b[base + 0x1000 * i..base + 0x1000 * (i + 1)]),
            _ => [&t.out8, &t.out8, &t.out8],
        };
        let halo = c.offsets(&rgb);
        let grain = c.grain();
        let frame = Frame {
            rgb: &rgb, width: c.width(), height: c.height(), clip: c.src.clip,
            matrix: c.matrix().map(|r| r.map(|v| v as f32)), tables: &t, out8, cube: &cube,
            gamut: c.m["case"]["gamut"].as_bool().unwrap(), halo: halo.as_ref(),
            grain: grain.as_ref().map(|(g, n)| (g, n.as_slice())),
        };
        let t0 = std::time::Instant::now();
        let out = frame.develop();
        let ms = t0.elapsed().as_millis();
        let (d, n) = diff(&out, &c.file("out.bin"));
        println!("{:14} out: max diff {d}, {n} of {} differ ({:.5}%), develop {ms} ms", c.name, out.len(), 100.0 * n as f64 / out.len() as f64);
        // Not 0: see output_exact_from_python_linear. A B&W paper's steep curve can turn that
        // one-ULP rounding into 2 levels on a handful of values.
        assert!(d <= 2.0, "{}: output differs by {d}", c.name);
        assert!((n as f64) < out.len() as f64 * 2e-5, "{}: {n} values differ", c.name);
    }
}
