#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["numpy", "rawpy", "pillow"]
# ///
"""Develop a Canon EOS 5D .CR2 (or another raw, e.g. Sony .ARW) into a .jpg with a Latent film profile.

    uv run analysis/tools/latent_develop.py IMG_0002.CR2 P400 [--paper 0] [--push 0] [--grade 5] [--kelvin K] [--half]
    uv run analysis/tools/latent_develop.py DSC08975.ARW P400 [--matrix adobe|dxo|9 numbers] [--exposure EV]

Reimplements the AUTOEXEC.BIN pipeline (see analysis/README.md, section 5):
  raw  -> (raw - 127) / (3692 - 127), per-channel clip at n_c / max(n)   FUN_0098c00c
       -> demosaic (LibRaw AHD; the firmware uses a 1-3-3-9 2x2 filter)
       -> camera matrix, WB via Bradford, x 2^(0.25 + 0.943)            FUN_009944b0
       -> + halation offsets (motion-picture films)                      FUN_00995d5c
       -> gamut compression with the pack's per-channel curves           FUN_00994c68
       -> LTP shaper -> tetrahedral 65^3 cube, blended between the two
          light (mired) steps around the shot's white balance            FUN_0098c00c
       -> + colour grain (17^3 lattice, 16-bit)                          FUN_009928e8
       -> 4096-entry 8-bit output table (B&W papers: per-channel LTVC
          curves chosen by paper, push and grade)                        FUN_009865ac
       -> + mono grain (8-bit amplitude table)                           FUN_009928e8
       -> JPEG
Grain and halation live in latent_effects.py.
LibRaw (rawpy) is used only to decode the lossless-JPEG raw data and demosaic.
Not reproduced: negative-value fine shaper, the firmware's own WB -> mired
table (approximated here with McCamy's CCT formula; --kelvin sets the light
directly, as the camera's Kelvin light mode does); halation is collected from
demosaiced pixels instead of raw photosites, see README section 5.3.
"""
import argparse
import math
import os
import struct
import sys
import time

import numpy as np
import rawpy
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import latent_color  # noqa: E402
import latent_crypt  # noqa: E402
import latent_effects  # noqa: E402
import latent_ltp  # noqa: E402

BLACK, WHITE = 127, 3692          # camera table at 0x9A9080
COLORDATA_WB_INDEX = 0x3F         # FUN_0098d29c, ColorData of 796 entries (5D)
CANON_CROP = (10, 6, 4368, 2912)  # visible area -> Canon's 4368x2912 frame


# ---------------------------------------------------------------- CR2 metadata

def read_cr2_meta(path):
    d = open(path, "rb").read(1 << 20)
    if d[:4] != b"II*\0" or d[8:10] != b"CR":
        raise ValueError("not a CR2 file")
    u16 = lambda o: struct.unpack_from("<H", d, o)[0]
    u32 = lambda o: struct.unpack_from("<I", d, o)[0]

    def ifd(off):
        return {struct.unpack_from("<H", d, off + 2 + 12 * i)[0]:
                struct.unpack_from("<HII", d, off + 4 + 12 * i) for i in range(u16(off))}

    ifd0 = ifd(u32(4))
    _, cnt, off = ifd0[0x110]
    model = d[off:off + cnt].rstrip(b"\0").decode("ascii")
    orientation = ifd0.get(0x112, (3, 1, 1))[2] & 0xFFFF
    exif = ifd(ifd0[0x8769][2])
    maker = ifd(exif[0x927C][2])
    _, cnt, off = maker[0x4001]
    if cnt != 796:
        raise ValueError("unsupported ColorData version (%d entries)" % cnt)
    r, g1, g2, b = struct.unpack_from("<4H", d, off + 2 * COLORDATA_WB_INDEX)
    return {"model": model, "orientation": orientation, "wb": (r, g1, g2, b)}


def load_rgb(path, half=False, black=BLACK, white=WHITE, crop=CANON_CROP):
    """Demosaiced camera RGB (uint16, 65535 = white), cropped to the camera's frame."""
    with rawpy.imread(path) as raw:
        rgb = raw.postprocess(
            output_color=rawpy.ColorSpace.raw, gamma=(1, 1), no_auto_bright=True,
            use_camera_wb=False, use_auto_wb=False, user_wb=[1, 1, 1, 1],
            user_black=black, user_sat=white, output_bps=16, half_size=half,
            highlight_mode=rawpy.HighlightMode.Clip, user_flip=0)
    s = 2 if half else 1
    x, y, cw, ch = (v // s for v in crop)
    return rgb[y:y + ch, x:x + cw]


# ---------------------------------------------------------------- other cameras
#
# The firmware only knows the 5D. For any other raw LibRaw supplies what the
# camera table holds: Adobe's XYZ->camera ColorMatrix, black/white levels, the
# as-shot WB and the camera's own crop. That feeds the same FUN_009944b0 build
# (Bradford to D65, x 2^(BaselineExposure + 0.943)), so the cube sees the same
# scene-referred Rec.709 input it gets from a 5D.

# BaselineExposure (EV) per model, i.e. where metered grey lands relative to
# sensor clip. The 5D's +0.25 is in the firmware table; others default to 0
# (tune with --exposure). The 1D X borrows the 5D's value (not measured).
BASELINE_EXPOSURE = {"ILCE-7M3": 0.0, "Canon EOS-1D X": 0.25}

# DxOMark "colour response" matrices: white-balanced raw -> linear sRGB under
# D50 (rows = sRGB R, G, B; each row sums to 1).
DXO_MATRICES = {
    "ILCE-7M3": ((2.10, -0.96, -0.14), (-0.17, 1.58, -0.41), (0.04, -0.50, 1.46)),
    "Canon EOS-1D X": ((2.22, -1.36, 0.15), (-0.26, 1.65, -0.39), (0.05, -0.75, 1.70)),
}

# DCIM folder names by model, then make; only used for the default grain seed path.
CARD_DIRS = {"Canon EOS-1D X": "100EOS1D", "Canon": "100EOS5D", "SONY": "100MSDCF"}


def _parse_tiff_ifd0(d):
    """Make / Model / Orientation from IFD0 of a TIFF; offsets are relative to the start of d."""
    e = {b"II": "<", b"MM": ">"}.get(d[:2])
    if e is None:
        raise ValueError("not a TIFF-based raw file")
    off = struct.unpack_from(e + "I", d, 4)[0]
    tags = {}
    for i in range(struct.unpack_from(e + "H", d, off)[0]):
        tag, typ, cnt, val = struct.unpack_from(e + "HHI4s", d, off + 2 + 12 * i)
        if typ == 2:
            o = struct.unpack(e + "I", val)[0] if cnt > 4 else None
            tags[tag] = (d[o:o + cnt] if o is not None else val[:cnt]).rstrip(b"\0").decode("ascii", "replace")
        elif typ == 3:
            tags[tag] = struct.unpack_from(e + "H", val)[0]
    return {"make": tags.get(0x10F, "").strip(), "model": tags.get(0x110, "").strip(),
            "orientation": tags.get(0x112, 1)}


def _find_cmt1(d):
    """Payload of a CR3's CMT1 box (a TIFF holding IFD0); the box size is the big-endian
    u32 right before the tag. d: the first MB of the file."""
    at = d.find(b"CMT1", 4)
    while at >= 0:
        size = struct.unpack_from(">I", d, at - 4)[0]
        end = at - 4 + size
        if size > 8 and end <= len(d) and d[at + 4:at + 6] in (b"II", b"MM"):
            return d[at + 4:end]
        at = d.find(b"CMT1", at + 1)
    return None


def read_tiff_meta(path):
    """Make / Model / Orientation: IFD0 of a TIFF-based raw (ARW, NEF, DNG, ...), else the
    CR3's CMT1 box. core/src/meta.rs read_meta does the same, then falls back to LibRaw."""
    with open(path, "rb") as f:
        d = f.read(1 << 20)
    try:
        return _parse_tiff_ifd0(d[:1 << 16])
    except (ValueError, struct.error):
        pass
    cmt1 = _find_cmt1(d)
    if cmt1 is not None:
        try:
            return _parse_tiff_ifd0(cmt1)
        except (ValueError, struct.error):
            pass
    raise ValueError("unsupported raw container")


def open_raw(path, autoexec, matrix="adobe", exposure=0.0):
    """Everything about a raw file that the pipeline needs before develop().

    matrix: "adobe" (ColorMatrix + Bradford, as the firmware does), "dxo"
    (DXO_MATRICES preset) or 9 numbers of a WB'd-raw -> linear sRGB matrix.
    exposure: EV added to the camera's BaselineExposure.
    Returns a dict; src["load"](half) gives the demosaiced RGB."""
    meta = read_tiff_meta(path)
    model = meta["model"]
    fw = latent_color.camera_table(latent_color._payload(autoexec))
    if open(path, "rb").read(10)[8:10] == b"CR" and model == fw[0]:
        # The camera the firmware table is for (the 5D): its own matrix, levels and WB words.
        meta = read_cr2_meta(path)
        meta["make"] = "Canon"
        _, cm, black, white, baseline = fw
        wb = meta["wb"][0], meta["wb"][1], meta["wb"][3]
        crop = CANON_CROP
    else:
        with rawpy.imread(path) as raw:
            cm = np.asarray(raw.rgb_xyz_matrix[:3], np.float64).tolist()
            if not any(any(r) for r in cm):
                raise ValueError("LibRaw has no colour matrix for %r" % model)
            if raw.num_colors != 3 or sorted(set(raw.raw_pattern.flatten())) != [0, 1, 2, 3]:
                raise ValueError("only RGGB-type Bayer sensors are supported")
            black = int(round(np.mean(raw.black_level_per_channel)))
            cam_white = raw.camera_white_level_per_channel
            white = int(min(cam_white)) if cam_white is not None and min(cam_white) > 0 else raw.white_level
            cwb = raw.camera_whitebalance
            wb = (cwb[0], cwb[1], cwb[2]) if cwb[0] > 0 and cwb[2] > 0 else tuple(raw.daylight_whitebalance[:3])
            # LibRaw's crop is relative to the full sensor, postprocess() returns the visible area.
            # A crop without a position reads as margin 0, which would land before that area.
            s = raw.sizes
            crop = ((max(s.crop_left_margin - s.left_margin, 0), max(s.crop_top_margin - s.top_margin, 0),
                     s.crop_width, s.crop_height)
                    if s.crop_width else (0, 0, s.width, s.height))
        meta["wb"] = (wb[0], wb[1], wb[1], wb[2])
        baseline = BASELINE_EXPOSURE.get(model, 0.0)
    wb_r, wb_g, wb_b = wb
    baseline += exposure
    if matrix == "adobe":
        m = latent_color.colour_matrix(autoexec, cm, baseline, wb_r, wb_g, wb_b)["matrix"]
    else:
        if matrix == "dxo":
            if model not in DXO_MATRICES:
                raise ValueError("no DxOMark matrix for %r (have: %s)" % (model, ", ".join(DXO_MATRICES)))
            srgb = np.asarray(DXO_MATRICES[model], np.float64)
        else:
            srgb = np.asarray(matrix, np.float64).reshape(3, 3)
        # Raw (1.0 = clip) -> WB gains -> sRGB matrix -> exposure.
        gains = np.array([wb_r / wb_g, 1.0, wb_b / wb_g])
        m = (2 ** (baseline + latent_color.LATENT_EXPOSURE_OFFSET) * srgb * gains).tolist()
    neutral = np.array([wb_g / wb_r, 1.0, wb_g / wb_b], dtype=np.float32)
    return {"meta": meta, "matrix": m, "clip": neutral / neutral.max(),
            "mired": scene_mired(cm, meta["wb"]), "baseline_exposure": baseline,
            "card_dir": CARD_DIRS.get(meta["model"], CARD_DIRS.get(meta["make"], "100CAMERA")),
            "load": lambda half=False: load_rgb(path, half, black, white, crop)}


# ---------------------------------------------------------------- light axis

def scene_mired(cm, wb):
    """Approximate the firmware's WB -> mired step (FUN_0099a0f8). cm: XYZ->camera."""
    x, y, z = latent_color.mv(latent_color.inv(cm), [wb[1] / wb[0], 1.0, wb[1] / wb[3]])
    cx, cy = x / (x + y + z), y / (x + y + z)
    n = (cx - 0.3320) / (0.1858 - cy)
    cct = 449 * n ** 3 + 3525 * n ** 2 + 6823.3 * n + 5520.33
    return 1e6 / cct


def light_steps(mireds, m):
    if m <= mireds[0]:
        return 0, 0, 0.0
    if m >= mireds[-1]:
        last = len(mireds) - 1
        return last, last, 0.0
    j = next(i for i, v in enumerate(mireds) if v >= m)
    return j - 1, j, (m - mireds[j - 1]) / (mireds[j] - mireds[j - 1])


# ---------------------------------------------------------------- film pack

def load_cube16(pack, idx):
    if hasattr(pack, "cube16"):     # latent_lite.LitePack
        return pack.cube16(idx)
    n, words = pack.cube(idx)
    w = np.array(words, dtype=np.uint32)
    r, g, b = w & 0x7FF, (w >> 11) & 0x7FF, w >> 22
    c = np.stack([(r << 5) | (r >> 6), (g << 5) | (g >> 6), (b << 6) | (b >> 4)], axis=-1)
    return c.reshape(n, n, n, 3).astype(np.float32)   # [b][g][r][channel], 0..65535


def load_grain(pack, latent_dir, key, paper_i, push_i, level):
    if hasattr(pack, "grain_block"):    # latent_lite.LitePack
        blk = pack.grain_block(paper_i, push_i)
    else:
        blk = latent_effects.read_grain_block(os.path.join(latent_dir, pack.code, "GRAIN.LTG"), key, paper_i, push_i)
    return latent_effects.Grain(blk, level)


def select_cubes(pack, paper_i, push_stops, grade, mired):
    """Returns (cube_i0, cube_i1, weight, paper_curves or None)."""
    if not 0 <= paper_i < len(pack.grade):
        raise ValueError("paper index must be 0..%d" % (len(pack.grade) - 1))
    if push_stops not in pack.push:
        raise ValueError("push must be one of %s" % pack.push)
    push_i = pack.push.index(push_stops)
    curves = None
    if pack.grade[paper_i] & 0x80:
        # FUN_0098533c: B&W papers (flag 0x80) share the cube row of the first
        # flagged paper at push index 0; the chosen paper, push and grade select
        # an LTVC block of per-channel output curves instead.
        if not 0 <= grade < len(pack.extra):
            raise ValueError("grade must be 0..%d" % (len(pack.extra) - 1))
        curves = pack.paper_curves(paper_i, push_i, grade)
        paper_i, push_i = next(i for i, g in enumerate(pack.grade) if g & 0x80), 0
    i0, i1, w = light_steps(pack.light_mired, mired)
    base = latent_ltp.cube_index(pack, 0, push_i, paper_i)
    return base + i0, base + i1, w, curves


# ---------------------------------------------------------------- pipeline

GAMUT_THRESHOLDS = np.array([0xD0A, 0xCD9, 0xE14], np.float32) / 4096   # 0x9A8CC4: 0.815/0.803/0.880


def gamut_compress(lin, curves):
    """FUN_00994c68 / FUN_00991240. For each channel, d = (max - c) / max; past
    the threshold c becomes max * (1 - curve_c(d)), curve_c being the pack's
    1024-entry table indexed by d in Q12 / 16. Always applied when max >= 8.0."""
    mx = lin.max(axis=-1, keepdims=True)
    safe = np.where(mx > 0, mx, 1.0)
    d = (mx - lin) / safe
    mask = (mx > 0) & ((d > GAMUT_THRESHOLDS) | (mx >= 8.0))
    if not mask.any():
        return lin
    r = np.floor(d * 4096)
    idx = np.clip((r // 16).astype(np.int32), 0, 0x3FF)
    frac = r - (r // 16) * 16
    out = lin.copy()
    for c in range(3):
        t = curves[c]
        i = idx[..., c]
        v = np.where(i < 0x3FF, t[i] + (frac[..., c] * (t[np.minimum(i + 1, 0x3FF)] - t[i]) + 8) // 16, t[0x3FF])
        comp = mx[..., 0] - mx[..., 0] * v / 4096
        out[..., c] = np.where(mask[..., c], comp, lin[..., c])
    return out


def shaper(lin, table):
    """Linear (1.0 = 0x10000) -> cube grid coordinate 0..64 (FUN_0098c00c, coarse table)."""
    v = np.clip(lin * 65536.0, 0, 0x80000 - 1) / 16.0
    i = np.floor(v).astype(np.int32)
    f = v - i
    return (table[i] + f * (table[i + 1] - table[i])) / 512.0


def tetrahedral(cube, r, g, b):
    n = cube.shape[0]
    def split(c):
        i = np.minimum(np.floor(c).astype(np.int32), n - 2)
        return i, (c - i)[..., None]
    ir, fr = split(r)
    ig, fg = split(g)
    ib, fb = split(b)
    at = lambda db, dg, dr: cube[ib + db, ig + dg, ir + dr]
    c000, c111 = at(0, 0, 0), at(1, 1, 1)
    out = np.empty(c000.shape, np.float32)
    cases = [
        (fr >= fg) & (fg >= fb), (fr >= fb) & (fb > fg), (fb > fr) & (fr >= fg),
        (fg > fr) & (fr >= fb), (fg >= fb) & (fb > fr), (fb > fg) & (fg > fr),
    ]
    # Each tetrahedron walks c000 -> one axis -> two axes -> c111.
    steps = [((0, 0, 1), (0, 1, 1), fr, fg, fb), ((0, 0, 1), (1, 0, 1), fr, fb, fg),
             ((1, 0, 0), (1, 0, 1), fb, fr, fg), ((0, 1, 0), (0, 1, 1), fg, fr, fb),
             ((0, 1, 0), (1, 1, 0), fg, fb, fr), ((1, 0, 0), (1, 1, 0), fb, fg, fr)]
    for mask, (a, bb, t1, t2, t3) in zip(cases, steps):
        m = mask[..., 0]
        if not m.any():
            continue
        ca, cb = at(*a)[m], at(*bb)[m]
        x1, x2, x3 = t1[m], t2[m], t3[m]
        out[m] = c000[m] + x1 * (ca - c000[m]) + x2 * (cb - ca) + x3 * (c111[m] - cb)
    return out


def develop(rgb_cam, clip, matrix, tables, cube, paper_curves=None, gamut=True,
            halo_offsets=None, grain=None, noise=None, rows=256):
    shaper_t = np.asarray(tables[:0x8001], dtype=np.float32)
    gamut_t = [np.asarray(tables[s:s + 0x400], dtype=np.float32) for s in (0x8C01, 0x9001, 0x9401)]
    if paper_curves:
        out8 = [np.frombuffer(c, dtype=np.uint8) for c in paper_curves]
    else:
        out8 = [np.frombuffer(struct.pack("<2048H", *tables[0x8001:0x8801]), dtype=np.uint8)] * 3
    m = np.asarray(matrix, dtype=np.float32)
    out = np.empty(rgb_cam.shape, np.uint8)
    for y in range(0, rgb_cam.shape[0], rows):
        cam = np.minimum(rgb_cam[y:y + rows].astype(np.float32) / 65535.0, clip)
        lin = cam @ m.T
        if halo_offsets is not None:
            lin = lin + next(halo_offsets)
        if gamut:
            lin = gamut_compress(lin, gamut_t)
        g = shaper(lin, shaper_t)
        o16 = np.clip(tetrahedral(cube, g[..., 0], g[..., 1], g[..., 2]), 0, 65535)
        n = noise[:, y:y + rows] if grain is not None else None
        if grain is not None:
            o16 = grain.apply16(o16, n, tetrahedral)
        i12 = (o16.astype(np.uint32) * 0xFFF + 0x8000) >> 16
        for c in range(3):
            out[y:y + rows, :, c] = out8[c][i12[..., c]]
        if grain is not None:
            out[y:y + rows] = grain.apply8(out[y:y + rows], n)
    return out


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.abspath(os.path.join(here, "..", ".."))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("raw", help="raw file: Canon EOS 5D .CR2, or another camera's raw that LibRaw "
                                "has a colour matrix for (e.g. Sony .ARW)")
    ap.add_argument("film", help="film code, e.g. P400 (LATENT/<code>.LTP)")
    ap.add_argument("--paper", type=int, default=0, help="paper / print stock index (default 0)")
    ap.add_argument("--push", type=int, default=0, help="push(+)/pull(-) value on the film's push axis (default 0)")
    ap.add_argument("--grade", type=int, default=5, help="B&W paper contrast grade index 0..11 (default 5)")
    ap.add_argument("--kelvin", type=float,
                    help="light colour temperature in K (default: estimated from the shot's white balance)")
    ap.add_argument("--matrix", default="adobe",
                    help="camera colour matrix: adobe (default, ColorMatrix + Bradford like the firmware), "
                         "dxo (DxOMark preset), or 9 comma-separated numbers of a WB'd-raw -> linear sRGB "
                         "matrix, row by row")
    ap.add_argument("--exposure", type=float, default=0.0,
                    help="EV added to the camera's BaselineExposure (default 0)")
    ap.add_argument("--no-gamut", action="store_true", help="skip the gamut compression step")
    ap.add_argument("--no-grain", action="store_true", help="skip film grain")
    ap.add_argument("--halation", type=int, choices=(0, 1, 2), default=2,
                    help="halation setting 0..2 -> strength 85/171/256 of 256 (default 2; motion-picture films only)")
    ap.add_argument("--no-halation", action="store_true", help="skip halation")
    ap.add_argument("--card-path", help="path the camera would use for this raw, which seeds the grain "
                                        "(default A:/DCIM/100EOS5D/<RAW NAME>, 100MSDCF for Sony)")
    ap.add_argument("--half", action="store_true", help="half-size output (faster)")
    ap.add_argument("--quality", type=int, default=95)
    ap.add_argument("--autoexec", default=os.path.join(repo, "AUTOEXEC.BIN"))
    ap.add_argument("--latent-dir", default=os.path.join(repo, "LATENT"))
    ap.add_argument("-o", "--output", help="output path (default: <raw name>_<film>.jpg next to the raw, "
                    "with _1, _2, ... appended if that name is taken)")
    a = ap.parse_args()

    t0 = time.time()
    matrix = a.matrix if a.matrix in ("adobe", "dxo") else [float(v) for v in a.matrix.split(",")]
    if not isinstance(matrix, str) and len(matrix) != 9:
        raise ValueError("--matrix needs 9 numbers")
    src = open_raw(a.raw, a.autoexec, matrix, a.exposure)
    meta = src["meta"]
    key = latent_crypt.load_key(a.autoexec)
    pack = latent_ltp.LTPack(os.path.join(a.latent_dir, a.film.upper() + ".LTP"), key)
    if not pack.index_checksum_ok:
        raise ValueError("film pack index checksum mismatch")
    if a.kelvin is not None and a.kelvin <= 0:
        raise ValueError("--kelvin must be positive")
    mired = 1e6 / a.kelvin if a.kelvin else src["mired"]
    i0, i1, w, paper_curves = select_cubes(pack, a.paper, a.push, a.grade, mired)
    cube = load_cube16(pack, i0)
    if i1 != i0 and w > 0:
        cube = (1 - w) * cube + w * load_cube16(pack, i1)
    tables = pack.tables()

    clip = src["clip"]
    rgb = src["load"](a.half)

    payload = latent_color._payload(a.autoexec)
    height, width = rgb.shape[:2]
    halo, halo_offsets = None, None
    profile = latent_effects.halation_profile(payload, pack.code)
    if profile is not None and not a.no_halation:
        halo = latent_effects.Halation(payload, profile, src["matrix"], clip,
                                       latent_effects.HALO_LEVELS[a.halation])
        maps = halo.build_map(rgb.astype(np.float32) / 65535.0)
        halo_offsets = halo.offsets(maps, height, width)
    grain, noise, card_path = None, None, None
    if not a.no_grain:
        # The grain block uses the selected paper and push, before the B&W cube remap.
        grain = load_grain(pack, a.latent_dir, key, a.paper, pack.push.index(a.push), a.grade)
        card_path = a.card_path or "A:/DCIM/%s/" % src["card_dir"] + os.path.basename(a.raw).upper()
        noise = latent_effects.grain_noise(payload, latent_effects.fnv1a_seed(card_path),
                                           height, width, grain.streams())

    img = develop(rgb, clip, src["matrix"], tables, cube, paper_curves, gamut=not a.no_gamut,
                  halo_offsets=halo_offsets, grain=grain, noise=noise)
    pil = Image.fromarray(img)
    pil = {3: pil.transpose(Image.ROTATE_180), 6: pil.transpose(Image.ROTATE_270),
           8: pil.transpose(Image.ROTATE_90)}.get(meta["orientation"], pil)
    out = a.output
    if not out:
        stem = "%s_%s" % (os.path.splitext(a.raw)[0], pack.code)
        out, n = stem + ".jpg", 1
        while os.path.exists(out):
            out, n = "%s_%d.jpg" % (stem, n), n + 1
    exif = Image.Exif()
    exif[0x010F], exif[0x0110] = meta["make"], meta["model"]
    exif[0x0131] = "Latent pipeline reimplementation"
    exif[0x010E] = "%s paper#%d push%+d%s %.0fK" % (
        pack.code, a.paper, a.push, " grade%d" % a.grade if paper_curves else "", 1e6 / mired)
    pil.save(out, quality=a.quality, exif=exif)

    print("%s: %s %s, WB RGGB %s -> %.1f mired (%.0f K%s)" % (
        a.raw, meta["make"], meta["model"], meta["wb"], mired, 1e6 / mired, ", --kelvin" if a.kelvin else ""))
    print("matrix %s, BaselineExposure %+.2f EV:" % (
        a.matrix if isinstance(matrix, str) else "custom", src["baseline_exposure"]))
    for row in src["matrix"]:
        print("  % .4f % .4f % .4f" % tuple(row))
    print("film %s, paper %d, push %+d%s: cubes %d/%d weight %.3f" % (
        pack.code, a.paper, a.push, ", B&W grade %d (LTVC)" % a.grade if paper_curves else "", i0, i1, w))
    if grain is not None:
        print("grain: %s %s, seed from %s" % ("mono" if grain.mono else "colour",
                                             "8-bit table" if grain.simple else "17^3 lattice", card_path))
    print("halation: %s" % ("strength %d/256" % latent_effects.HALO_LEVELS[a.halation] if halo
                            else "off" if a.no_halation else "not used by this film"))
    print("wrote %s (%dx%d) in %.1fs" % (out, pil.width, pil.height, time.time() - t0))


if __name__ == "__main__":
    sys.exit(main())
