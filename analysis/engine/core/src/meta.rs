//! What the pipeline needs to know about a raw before develop(): latent_develop.open_raw.
use crate::color::{camera_table, colour_matrix, inv, mv, M3};
use crate::firmware::Firmware;
use crate::Result;

const COLORDATA_WB_INDEX: usize = 0x3F;
const CANON_CROP: [usize; 4] = [10, 6, 4368, 2912];

/// LibRaw's side of a raw, as rawpy reports it. Filled by the wasm crate.
#[derive(Clone, Debug, Default)]
pub struct RawInfo {
    pub colors: i32,
    pub filters: u32,
    /// black + cblack[c].
    pub black_level_per_channel: [u32; 4],
    pub linear_max: [u32; 4],
    pub maximum: u32,
    pub cam_mul: [f32; 4],
    pub pre_mul: [f32; 4],
    pub cam_xyz: [[f32; 3]; 3],
    pub width: u32,
    pub height: u32,
    pub top_margin: u32,
    pub left_margin: u32,
    /// cleft, ctop, cwidth, cheight. 0xFFFF in the first two means no position.
    pub crop: [u32; 4],
    /// LibRaw's make / model / flip: only used when the file carries neither a TIFF IFD0
    /// nor a CR3 CMT1 box.
    pub make: String,
    pub model: String,
    pub flip: i32,
}

#[derive(Debug)]
pub struct Source {
    pub make: String,
    pub model: String,
    pub orientation: u32,
    pub wb: [f64; 4],
    pub matrix: M3,
    pub clip: [f32; 3],
    pub mired: f64,
    pub baseline_exposure: f64,
    pub card_dir: &'static str,
    pub black: i32,
    pub white: i32,
    /// x, y, width, height of the frame in the decoded image.
    pub crop: [usize; 4],
}

fn u16_at(d: &[u8], o: usize, big: bool) -> Result<u16> {
    let b: [u8; 2] = d.get(o..o + 2).ok_or("truncated TIFF header")?.try_into().unwrap();
    Ok(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
}

fn u32_at(d: &[u8], o: usize, big: bool) -> Result<u32> {
    let b: [u8; 4] = d.get(o..o + 4).ok_or("truncated TIFF header")?.try_into().unwrap();
    Ok(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

fn ascii(b: &[u8]) -> String {
    let end = b.iter().rposition(|c| *c != 0).map_or(0, |i| i + 1);
    b[..end].iter().map(|c| if c.is_ascii() { *c as char } else { char::REPLACEMENT_CHARACTER }).collect()
}

struct TiffMeta {
    make: String,
    model: String,
    orientation: u32,
}

/// Make / Model / Orientation from IFD0 of a TIFF. Offsets are relative to the start of `d`.
fn parse_tiff_ifd0(d: &[u8]) -> Result<TiffMeta> {
    let big = match d.get(..2) {
        Some(b"II") => false,
        Some(b"MM") => true,
        _ => return Err("not a TIFF-based raw file".into()),
    };
    let off = u32_at(d, 4, big)? as usize;
    let mut m = TiffMeta { make: String::new(), model: String::new(), orientation: 1 };
    for i in 0..u16_at(d, off, big)? as usize {
        let e = off + 2 + 12 * i;
        let (tag, typ, cnt) = (u16_at(d, e, big)?, u16_at(d, e + 2, big)?, u32_at(d, e + 4, big)? as usize);
        let text = |o: usize, n: usize| ascii(d.get(o..).map_or(&[], |t| &t[..n.min(t.len())]));
        let value = match typ {
            2 if cnt > 4 => text(u32_at(d, e + 8, big)? as usize, cnt),
            2 => text(e + 8, cnt),
            _ => String::new(),
        };
        match (tag, typ) {
            (0x10F, 2) => m.make = value.trim().to_string(),
            (0x110, 2) => m.model = value.trim().to_string(),
            (0x112, 3) => m.orientation = u16_at(d, e + 8, big)? as u32,
            _ => {}
        }
    }
    Ok(m)
}

/// The payload of a CR3's `CMT1` box (a TIFF holding IFD0), searched for in the first MB:
/// the box size is the big-endian u32 right before the tag.
fn find_cmt1(data: &[u8]) -> Option<&[u8]> {
    let d = &data[..data.len().min(1 << 20)];
    let mut from = 4;
    while let Some(i) = d.get(from..)?.windows(4).position(|w| w == b"CMT1") {
        let at = from + i;
        let size = u32::from_be_bytes(d[at - 4..at].try_into().unwrap()) as usize;
        let end = (at - 4).checked_add(size);
        if let Some(end) = end.filter(|e| size > 8 && *e <= d.len()) {
            if matches!(d.get(at + 4..at + 6), Some(b"II" | b"MM")) {
                return Some(&d[at + 4..end]);
            }
        }
        from = at + 1;
    }
    None
}

/// LibRaw's flip back to a TIFF Orientation; LibRaw maps Orientation o to `"50132467"[o & 7]`.
fn flip_to_orientation(flip: i32) -> u32 {
    match flip {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 3,
        4 => 5,
        5 => 8,
        6 => 6,
        7 => 7,
        _ => 1,
    }
}

/// Make / Model / Orientation: IFD0 of a TIFF-based raw (ARW, NEF, DNG, ...), else the CR3's
/// CMT1 box, else what LibRaw found.
/// LibRaw's make/model are normalised ("Sony", model without the make), so they may not match
/// the IFD0 strings that `card_dir` and `baseline_for` compare against.
fn read_meta(data: &[u8], info: &RawInfo) -> TiffMeta {
    parse_tiff_ifd0(&data[..data.len().min(1 << 16)])
        .or_else(|_| find_cmt1(data).ok_or(String::new()).and_then(parse_tiff_ifd0))
        .unwrap_or_else(|_| TiffMeta {
            make: info.make.trim().to_string(),
            model: info.model.trim().to_string(),
            orientation: flip_to_orientation(info.flip),
        })
}

/// The 5D's own: orientation and the WB words of ColorData.
fn read_cr2_meta(data: &[u8]) -> Result<(u32, [u16; 4])> {
    let d = &data[..data.len().min(1 << 20)];
    let find = |off: usize, tag: u16| -> Result<Option<(usize, u32)>> {
        for i in 0..u16_at(d, off, false)? as usize {
            let e = off + 2 + 12 * i;
            if u16_at(d, e, false)? == tag {
                return Ok(Some((u32_at(d, e + 4, false)? as usize, u32_at(d, e + 8, false)?)));
            }
        }
        Ok(None)
    };
    let ifd0 = u32_at(d, 4, false)? as usize;
    let orientation = find(ifd0, 0x112)?.map_or(1, |(_, v)| v & 0xFFFF);
    let exif = find(ifd0, 0x8769)?.ok_or("CR2 without EXIF")?.1 as usize;
    let maker = find(exif, 0x927C)?.ok_or("CR2 without MakerNote")?.1 as usize;
    let (cnt, off) = find(maker, 0x4001)?.ok_or("CR2 without ColorData")?;
    if cnt != 796 {
        return Err(format!("unsupported ColorData version ({cnt} entries)"));
    }
    let word = |i: usize| u16_at(d, off as usize + 2 * (COLORDATA_WB_INDEX + i), false);
    Ok((orientation, [word(0)?, word(1)?, word(2)?, word(3)?]))
}

/// Approximates the firmware's WB -> mired step with McCamy's CCT. cm: XYZ->camera.
fn scene_mired(cm: &M3, wb: [f64; 4]) -> f64 {
    let [x, y, z] = mv(&inv(cm), [wb[1] / wb[0], 1.0, wb[1] / wb[3]]);
    let (cx, cy) = (x / (x + y + z), y / (x + y + z));
    let n = (cx - 0.3320) / (0.1858 - cy);
    let cct = 449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33;
    1e6 / cct
}

fn baseline_for(model: &str) -> f64 {
    // BaselineExposure per model; the 1D X borrows the 5D's value.
    if model == "Canon EOS-1D X" { 0.25 } else { 0.0 }
}

fn card_dir(model: &str, make: &str) -> &'static str {
    match (model, make) {
        ("Canon EOS-1D X", _) => "100EOS1D",
        (_, "Canon") => "100EOS5D",
        (_, "SONY") => "100MSDCF",
        _ => "100CAMERA",
    }
}

/// `data` is the file; `info` comes from LibRaw, and the 5D's table replaces most of it.
pub fn open_raw(data: &[u8], fw: &Firmware, info: &RawInfo) -> Result<Source> {
    let tiff = read_meta(data, info);
    let table = camera_table(fw)?;
    let is_table_camera = data.get(8..10) == Some(b"CR") && tiff.model == table.model;
    let (make, orientation, cm, black, white, baseline, wb3, wb4, crop);
    if is_table_camera {
        let (o, words) = read_cr2_meta(data)?;
        make = "Canon".to_string();
        orientation = o;
        cm = table.color_matrix;
        (black, white) = (table.black as i32, table.white as i32);
        baseline = table.baseline;
        let w = words.map(|v| v as f64);
        wb3 = [w[0], w[1], w[3]];
        wb4 = w;
        crop = CANON_CROP;
    } else {
        make = tiff.make.clone();
        orientation = tiff.orientation;
        cm = info.cam_xyz.map(|r| r.map(|v| v as f64));
        if cm.iter().all(|r| r.iter().all(|v| *v == 0.0)) {
            return Err(format!("LibRaw has no colour matrix for {:?}", tiff.model));
        }
        let fc = |r: u32, c: u32| info.filters >> ((((r << 1) & 14) | (c & 1)) << 1) & 3;
        let mut pattern = [fc(0, 0), fc(0, 1), fc(1, 0), fc(1, 1)];
        pattern.sort();
        if info.colors != 3 || info.filters < 1000 || pattern != [0, 1, 2, 3] {
            return Err("only RGGB-type Bayer sensors are supported".into());
        }
        let mean = info.black_level_per_channel.iter().map(|v| *v as f64).sum::<f64>() / 4.0;
        black = mean.round_ties_even() as i32;
        let lowest = *info.linear_max.iter().min().unwrap();
        white = if lowest > 0 { lowest as i32 } else { info.maximum as i32 };
        let c = info.cam_mul;
        let w = if c[0] > 0.0 && c[2] > 0.0 { c } else { info.pre_mul };
        wb3 = [w[0] as f64, w[1] as f64, w[2] as f64];
        wb4 = [wb3[0], wb3[1], wb3[1], wb3[2]];
        // LibRaw's crop is relative to the full sensor, the decoded image is the visible area.
        // A crop without a position reads as margin 0, which would land before that area.
        let [cl, ct, cw, ch] = info.crop;
        let pos = |v: u32, margin: u32| if v == 0xFFFF { 0 } else { (v as i64 - margin as i64).max(0) as usize };
        crop = if cw != 0 {
            [pos(cl, info.left_margin), pos(ct, info.top_margin), cw as usize, ch as usize]
        } else {
            [0, 0, info.width as usize, info.height as usize]
        };
        baseline = baseline_for(&tiff.model);
    }
    let matrix = colour_matrix(fw, &cm, baseline, wb3)?;
    let neutral = [(wb3[1] / wb3[0]) as f32, 1.0, (wb3[1] / wb3[2]) as f32];
    let top = neutral.iter().cloned().fold(f32::MIN, f32::max);
    Ok(Source {
        card_dir: card_dir(&tiff.model, &make),
        make,
        model: tiff.model,
        orientation,
        wb: wb4,
        matrix,
        clip: neutral.map(|v| v / top),
        mired: scene_mired(&cm, wb4),
        baseline_exposure: baseline,
        black,
        white,
        crop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF whose IFD0 holds Make, Model and Orientation.
    fn tiff(make: &str, model: &str, orientation: u16) -> Vec<u8> {
        let mut d = b"II*\0".to_vec();
        d.extend(8u32.to_le_bytes());
        d.extend(3u16.to_le_bytes());
        let strings = 8 + 2 + 3 * 12 + 4;
        let mut tail = Vec::new();
        for (tag, s) in [(0x10Fu16, make), (0x110, model)] {
            let mut v = s.as_bytes().to_vec();
            v.push(0);
            d.extend(tag.to_le_bytes());
            d.extend(2u16.to_le_bytes());
            d.extend((v.len() as u32).to_le_bytes());
            d.extend(((strings + tail.len()) as u32).to_le_bytes());
            tail.extend(v);
        }
        d.extend(0x112u16.to_le_bytes());
        d.extend(3u16.to_le_bytes());
        d.extend(1u32.to_le_bytes());
        d.extend((orientation as u32).to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(tail);
        d
    }

    fn n_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(payload);
        b
    }

    /// ftyp, then moov/uuid holding a CMT1 box, like a CR3.
    fn cr3(cmt1: &[u8]) -> Vec<u8> {
        let mut uuid = vec![0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
        uuid.extend(n_box(b"CNCV", b"CanonCR3_001/00.09.00/00.00.00"));
        uuid.extend(n_box(b"CMT1", cmt1));
        let mut d = n_box(b"ftyp", b"crx \0\0\0\x01crx isom");
        d.extend(n_box(b"moov", &n_box(b"uuid", &uuid)));
        d.extend(vec![0xAB; 4096]);
        d
    }

    fn libraw() -> RawInfo {
        RawInfo { make: "Fooma".into(), model: "X 1".into(), flip: 5, ..Default::default() }
    }

    #[test]
    fn tiff_ifd0_wins() {
        let m = read_meta(&tiff("SONY", "ILCE-7M3", 6), &libraw());
        assert_eq!((m.make.as_str(), m.model.as_str(), m.orientation), ("SONY", "ILCE-7M3", 6));
    }

    #[test]
    fn cr3_reads_cmt1() {
        let data = cr3(&tiff("Canon", "Canon EOS RP", 8));
        assert_eq!(&data[8..10], b"cr");
        let m = read_meta(&data, &libraw());
        assert_eq!((m.make.as_str(), m.model.as_str(), m.orientation), ("Canon", "Canon EOS RP", 8));
    }

    #[test]
    fn cmt1_that_is_not_a_tiff_is_skipped() {
        let mut data = cr3(b"nope, not a TIFF");
        assert!(find_cmt1(&data).is_none());
        // The next CMT1 after a rejected one is found.
        data.truncate(data.len() - 4096);
        data.extend(n_box(b"CMT1", &tiff("Canon", "Canon EOS R5", 1)));
        assert_eq!(parse_tiff_ifd0(find_cmt1(&data).unwrap()).unwrap().model, "Canon EOS R5");
        let mut bad = b"\xff\xff\xff\xffCMT1II".to_vec();
        bad.extend(vec![0; 64]);
        assert!(find_cmt1(&bad).is_none());
        // A bogus size (u32::MAX) before a tag past offset 4 is skipped, and the valid box after it found.
        let mut bogus = n_box(b"ftyp", b"crx ");
        bogus.extend(u32::MAX.to_be_bytes());
        bogus.extend(b"CMT1II*\0");
        bogus.extend(n_box(b"CMT1", &tiff("Canon", "Canon EOS R6", 3)));
        let m = parse_tiff_ifd0(find_cmt1(&bogus).unwrap()).unwrap();
        assert_eq!((m.model.as_str(), m.orientation), ("Canon EOS R6", 3));
    }

    #[test]
    fn other_containers_use_libraw() {
        let m = read_meta(b"\0\0\0\x10FOOBlorem ipsum dolor", &libraw());
        assert_eq!((m.make.as_str(), m.model.as_str(), m.orientation), ("Fooma", "X 1", 8));
    }

    #[test]
    fn flip_table_inverts_libraw() {
        // LibRaw: flip = "50132467"[orientation & 7].
        for o in 1..=8u32 {
            let flip = (b"50132467"[(o & 7) as usize] - b'0') as i32;
            assert_eq!(flip_to_orientation(flip), o);
        }
        assert_eq!([0, 3, 5, 6].map(flip_to_orientation), [1, 3, 8, 6]);
        assert_eq!(flip_to_orientation(-1), 1);
    }
}
