//! The module's C API, wrapped by app/src/engine/wasm.ts. Calls that can fail return a
//! NUL-terminated JSON text: {"ok": ...}, {"error": message} or {"need": path}; a call's
//! binary result stays readable through engine_blob_ptr/len until the next call makes one.
mod raw;

use latent_core::meta::RawInfo;
use latent_core::studio::{parse_query, Decoder, Job, Stop, Studio};
use raw::Raw;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::ffi::{c_char, CStr};

/// A raw and the buffer LibRaw reads it from. raw goes first when dropped.
struct Opened {
    raw: Raw,
    data: Vec<u8>,
    /// Taken at open: lr_decode rewrites LibRaw's sizes and colour data (flip becomes user_flip).
    info: RawInfo,
}

impl Decoder for Opened {
    fn data(&self) -> &[u8] {
        &self.data
    }

    fn info(&self) -> RawInfo {
        self.info.clone()
    }

    fn decode(&mut self, half: bool, black: i32, white: i32) -> Result<(Vec<u16>, usize, usize), String> {
        self.raw.decode(half, black, white)
    }
}

/// LibRaw's side of an opened raw, before any decode.
fn raw_info(raw: &Raw) -> RawInfo {
    let m = raw.meta();
    RawInfo {
        colors: m.colors,
        filters: m.filters,
        black_level_per_channel: m.cblack.map(|c| m.black + c),
        linear_max: m.linear_max,
        maximum: m.maximum,
        cam_mul: m.cam_mul,
        pre_mul: m.pre_mul,
        cam_xyz: [m.cam_xyz[0], m.cam_xyz[1], m.cam_xyz[2]],
        width: m.width,
        height: m.height,
        top_margin: m.top_margin,
        left_margin: m.left_margin,
        crop: [m.crop_left, m.crop_top, m.crop_width, m.crop_height],
        make: c_text(&m.make),
        model: c_text(&m.model),
        flip: m.flip,
    }
}

/// A NUL-terminated char array of LrMeta.
fn c_text(b: &[u8]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn open(data: Vec<u8>) -> Result<Box<dyn Decoder>, String> {
    let raw = Raw::open(&data)?;
    let info = raw_info(&raw);
    Ok(Box::new(Opened { raw, data, info }))
}

#[derive(Default)]
struct State {
    studio: Option<Studio>,
    job: Option<Job>,
    text: Vec<u8>,
    blob: Vec<u8>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn reply(s: &mut State, v: Value) -> *const u8 {
    s.text = v.to_string().into_bytes();
    s.text.push(0);
    s.text.as_ptr()
}

fn answer(s: &mut State, r: Result<Value, Stop>) -> *const u8 {
    let v = match r {
        Ok(v) => json!({ "ok": v }),
        Err(Stop::Need(p)) => json!({ "need": p }),
        Err(Stop::Bad(e)) => json!({ "error": e }),
    };
    reply(s, v)
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with_borrow_mut(f)
}

unsafe fn text(p: *const c_char) -> String {
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// A buffer from engine_alloc, handed over.
unsafe fn owned(p: *mut u8, len: usize) -> Vec<u8> {
    Vec::from_raw_parts(p, len, len)
}

fn studio(s: &mut State) -> Result<&mut Studio, Stop> {
    s.studio.as_mut().ok_or_else(|| Stop::Bad("engine_init has not run".into()))
}

#[no_mangle]
pub extern "C" fn engine_alloc(size: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(size);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[no_mangle]
pub extern "C" fn engine_blob_ptr() -> *const u8 {
    with(|s| s.blob.as_ptr())
}

#[no_mangle]
pub extern "C" fn engine_blob_len() -> usize {
    with(|s| s.blob.len())
}

#[no_mangle]
pub unsafe extern "C" fn engine_init(firmware: *mut u8, len: usize) -> *const u8 {
    with(|s| {
        let r = Studio::new(owned(firmware, len), open).map(|st| s.studio = Some(st)).map(|_| Value::Null);
        answer(s, r.map_err(Stop::Bad))
    })
}

#[no_mangle]
pub unsafe extern "C" fn engine_add_film(film_json: *const u8, len: usize) -> *const u8 {
    with(|s| {
        let data = std::slice::from_raw_parts(film_json, len);
        let r = studio(s).and_then(|st| st.add_film(data).map_err(Stop::Bad)).map(|_| Value::Null);
        answer(s, r)
    })
}

#[no_mangle]
pub unsafe extern "C" fn engine_films() -> *const u8 {
    with(|s| {
        let r = studio(s).and_then(|st| st.films().map_err(Stop::Bad));
        answer(s, r)
    })
}

/// Takes the buffer. path: a pack file, or "raw:<file id>".
#[no_mangle]
pub unsafe extern "C" fn engine_put(path: *const c_char, data: *mut u8, len: usize) {
    let (path, data) = (text(path), owned(data, len));
    with(|s| {
        if let Some(st) = s.studio.as_mut() {
            st.put(path, data);
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn engine_clear_files() {
    with(|s| {
        if let Some(st) = s.studio.as_mut() {
            st.clear_files();
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn engine_forget(id: *const c_char) {
    let id = text(id);
    with(|s| {
        if let Some(st) = s.studio.as_mut() {
            st.forget(&id);
        }
    })
}

/// Takes the buffer. {"ok": null}, or why the file cannot be developed.
#[no_mangle]
pub unsafe extern "C" fn engine_check(data: *mut u8, len: usize) -> *const u8 {
    let data = owned(data, len);
    with(|s| {
        let r = studio(s).and_then(|st| st.check(data).map_err(Stop::Bad)).map(|_| Value::Null);
        answer(s, r)
    })
}

/// Takes the buffer. {"jpeg", "width", "height", "orientation"}, with the bytes in the blob.
#[no_mangle]
pub unsafe extern "C" fn engine_thumb(data: *mut u8, len: usize) -> *const u8 {
    let data = owned(data, len);
    with(|s| {
        let r = (|| {
            let mut raw = Raw::open(&data)?;
            let info = raw_info(&raw);
            let t = raw.thumb()?;
            let opened = Opened { raw, data, info };
            let orientation = s.studio.as_ref().ok_or("engine_init has not run")?.orientation(&opened)?;
            let v = json!({"jpeg": t.jpeg, "width": t.width, "height": t.height, "orientation": orientation});
            s.blob = t.data;
            Ok::<Value, String>(v)
        })();
        answer(s, r.map_err(Stop::Bad))
    })
}

/// Stage 1 of a render: everything but develop(). {"info", "name"}.
#[no_mangle]
pub unsafe extern "C" fn engine_prepare(query: *const c_char) -> *const u8 {
    let query = text(query);
    with(|s| {
        let r = (|| {
            let q = parse_query(&query)?;
            let job = studio(s)?.prepare(&q)?;
            let v = json!({"info": job.info, "name": job.name, "gpu": gpu_plan(&job)});
            s.job = Some(job);
            Ok(v)
        })();
        answer(s, r)
    })
}

fn resource<T>(tag: String, data: &[T]) -> Value {
    json!({"tag": tag, "ptr": data.as_ptr() as usize, "bytes": std::mem::size_of_val(data)})
}

/// What app/src/engine/gpu.ts needs. Pointers hold while the job does.
fn gpu_plan(job: &Job) -> Value {
    let img = &job.image.v;
    let g = &job.gpu;
    let grain = job.grain.as_ref();
    json!({
        "width": img.width, "height": img.height, "orientation": job.orientation,
        "matrix": job.matrix.map(|r| r.map(|v| v as f32)),
        "clip": job.clip, "gamut": job.gamut,
        "grain": grain.map(|(g, _)| if g.v.simple { "simple" } else { "lattice" }),
        "mono": grain.is_some_and(|(g, _)| g.v.mono),
        "halo": job.halo.as_ref().map(|(_, p, level)| {
            let s = (*level as f64 / 256.0) as f32;
            p.c.map(|row| row.map(|v| v * s))
        }),
        "res": {
            "rgb": resource(job.image.tag.to_string(), &img.rgb),
            "noise": grain.map(|(_, n)| resource(n.tag.to_string(), &n.v)),
            "cube": resource(job.cube.tag.to_string(), &job.cube.v),
            "tables": resource(job.tables.tag.to_string(), &job.tables.v.flat),
            "out8": resource(g.out8_tag.clone(), &g.out8),
            "grain": grain.map(|(g, _)| resource(g.tag.to_string(), &g.v.gpu)),
            "maps": job.halo.as_ref().map(|(m, _, _)| resource(m.tag.to_string(), &g.maps)),
        },
    })
}

/// Takes the buffer: RGBA8 of the prepared job, from the GPU. The JPEG lands in the blob.
#[no_mangle]
pub unsafe extern "C" fn engine_encode(rgba: *mut u8, len: usize, width: usize, height: usize) -> *const u8 {
    let rgba = owned(rgba, len);
    with(|s| {
        let r = (|| {
            let job = s.job.as_ref().ok_or("nothing prepared")?;
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            drop(rgba);
            s.blob = job.encode(rgb, width, height)?;
            Ok::<Value, String>(Value::Null)
        })();
        answer(s, r.map_err(Stop::Bad))
    })
}

/// Takes the buffer: the GPU's RGBA8 against the CPU path, for `?check=1`.
#[no_mangle]
pub unsafe extern "C" fn engine_compare(rgba: *mut u8, len: usize) -> *const u8 {
    let rgba = owned(rgba, len);
    with(|s| {
        let r = (|| {
            let job = s.job.as_ref().ok_or("nothing prepared")?;
            let cpu = job.develop();
            let mut histogram = [0u64; 8];
            let (mut max, mut differ, mut values) = (0, 0u64, 0u64);
            for (c, g) in cpu.chunks_exact(3).zip(rgba.chunks_exact(4)) {
                for i in 0..3 {
                    let d = c[i].abs_diff(g[i]) as usize;
                    max = max.max(d);
                    differ += (d != 0) as u64;
                    values += 1;
                    histogram[d.min(7)] += 1;
                }
            }
            Ok::<Value, String>(json!({"max": max, "differ": differ, "values": values, "histogram": histogram}))
        })();
        answer(s, r.map_err(Stop::Bad))
    })
}

/// The CPU path: develops the prepared job and leaves the JPEG in the blob.
#[no_mangle]
pub extern "C" fn engine_develop_cpu() -> *const u8 {
    with(|s| {
        let r = (|| {
            let job = s.job.as_ref().ok_or("nothing prepared")?;
            let rgb = job.develop();
            let img = &job.image.v;
            s.blob = job.encode(rgb, img.width, img.height)?;
            Ok::<Value, String>(Value::Null)
        })();
        answer(s, r.map_err(Stop::Bad))
    })
}
