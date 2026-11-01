// The darkroom's worker: holds the Rust + LibRaw module, the raws and the way to the packs.
// The folder the user picks is theirs and never leaves the machine. Lite packs come from
// that folder, or from a URL: then firmware.bin and every film.json are read now, the rest
// when the engine asks for it, so a session downloads only the papers it uses.
import { Gpu } from "@/engine/gpu";
import { loadEngine, type Engine, type Prepared, type Reply } from "@/engine/wasm";
import { RAW_FILE as RAW_RE, RAW_LABEL } from "@/lib/kit";
import type { KitFile, RawFile } from "@/lib/types";

const FILM_JSON = /(^|\/)([^/]+)\/film\.json$/;

interface Lite { base: string; files: Record<string, [number, string]>; cache: Cache | null }

let engine: Engine;
let gpu: Gpu | null = null;
let canvas: OffscreenCanvas | null = null;
let lite: Lite | null = null;
let readPack: (path: string) => Promise<Uint8Array>;   // paths as the engine names them: P400/tables.bin
const raws = new Map<string, File | Uint8Array>();

function unwrap<T>(r: Reply<T>): T {
  if ("error" in r) throw new Error(r.error);
  if ("need" in r) throw new Error(`the engine asked for ${r.need} outside a render`);
  return r.ok;
}

// ---------------------------------------------------------------- lite packs over HTTP

function liteUrl(path: string) { return `${lite!.base}${path}?v=${lite!.files[path][1]}`; }

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => {
  for (let k = 0; k < 8; k++) n = n & 1 ? 0xEDB88320 ^ (n >>> 1) : n >>> 1;
  return n;
});

// As index.json has it: zlib.crc32 in 8 hex digits.
function crc32(bytes: Uint8Array) {
  let c = -1;
  for (let i = 0; i < bytes.length; i++) c = CRC_TABLE[(c ^ bytes[i]) & 255] ^ (c >>> 8);
  return (~c >>> 0).toString(16).padStart(8, "0");
}

// Static hosts and dev servers tend to answer a missing path with index.html and a 200, so
// nothing is used or cached until it matches index.json; a bad cache entry goes away here.
async function fetchLite(path: string) {
  const l = lite!;
  if (!l.files[path]) throw new Error(`${path} is not in ${l.base}index.json`);
  const url = liteUrl(path);
  const [size, crc] = l.files[path];
  const hit = l.cache && await l.cache.match(url);
  let data: Uint8Array<ArrayBuffer> | null = hit ? new Uint8Array(await hit.arrayBuffer()) : null;
  if (data && crc32(data) !== crc) { await l.cache!.delete(url); data = null; }
  if (!data) {
    if (size > 1 << 20) self.postMessage({ status: `downloading ${path} (${(size / 1e6).toFixed(1)} MB)…` });
    const r = await fetch(url, { cache: "no-cache" });
    if (!r.ok) throw new Error(`${url}: ${r.status}`);
    const got = new Uint8Array(await r.arrayBuffer());
    if (crc32(got) !== crc) {
      throw new Error(`${url}: the server's file is damaged or missing (${got.length} bytes of ${r.headers.get("content-type")}, index.json says ${size})`);
    }
    if (l.cache) await l.cache.put(url, new Response(got)).catch(() => {});   // out of quota: it still works this once
    data = got;
  }
  return data;
}

// Returns the paths of the film.json files.
async function openLiteUrl(base: string) {
  const r = await fetch(base + "index.json", { cache: "no-cache" });
  if (!r.ok) throw new Error(`${base}index.json: ${r.status}`);
  const { files } = await r.json();
  // The cache only saves downloads: without one (it can fail to open) everything still works.
  const cache = await caches.open("latent-lite " + base).catch(() => null);
  lite = { base, files, cache };
  const current = new Set(Object.keys(files).map(liteUrl));
  if (cache) for (const req of await cache.keys()) if (!current.has(req.url)) cache.delete(req);
  readPack = fetchLite;
  return Object.keys(files).filter((p) => FILM_JSON.test(p));
}

// A LATENT_LITE folder among the picked files, or null.
function openLiteFolder(kit: KitFile[]) {
  const firmware = kit.map((k) => k.path).filter((p) => /(^|\/)firmware\.bin$/.test(p))
    .sort((a, b) => a.split("/").length - b.split("/").length)[0];
  if (firmware === undefined) return null;
  const root = firmware.slice(0, firmware.length - "firmware.bin".length);
  const byPath = new Map(kit.filter((k) => k.path.startsWith(root)).map((k) => [k.path.slice(root.length), k.file]));
  readPack = async (path) => {
    const f = byPath.get(path);
    if (!f) throw new Error(`${path} is not in the chosen folder`);
    return new Uint8Array(await f.arrayBuffer());
  };
  return [...byPath.keys()].filter((p) => FILM_JSON.test(p));
}

function fileList(): RawFile[] {
  return [...raws.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([id, f]) => ({ id, name: id.slice(2), size: f instanceof File ? f.size : f.length, uploaded: id[0] === "u" }));
}

async function rawBytes(id: string) {
  const f = raws.get(id);
  if (!f) throw new Error(`file not found: ${id.slice(2)}`);
  return f instanceof File ? new Uint8Array(await f.arrayBuffer()) : f;
}

// A call that stops on a file it does not have yet: get it, call again. The engine keeps
// what it decodes, so the files only stay for the call.
async function withFiles<T>(fn: () => Reply<T>) {
  try {
    for (;;) {
      const r = fn();
      if (!("need" in r)) return unwrap(r);
      engine.put(r.need, r.need.startsWith("raw:") ? await rawBytes(r.need.slice(4)) : await readPack(r.need));
    }
  } finally {
    engine.clearFiles();
  }
}

// ---------------------------------------------------------------- thumbnails

const encodeJpeg = (c: OffscreenCanvas) => c.convertToBlob({ type: "image/jpeg", quality: 0.8 });

// Fits 320 px and stands the picture up, as Studio.thumb. Previews are landscape as stored,
// so one that arrives portrait has had its own EXIF applied by the browser.
async function shrink(src: ImageBitmap, orientation: number) {
  const scale = Math.min(1, 320 / Math.max(src.width, src.height));
  const [w, h] = [Math.round(src.width * scale), Math.round(src.height * scale)];
  const turn = (orientation === 6 ? 90 : orientation === 8 ? -90 : orientation === 3 ? 180 : 0);
  const quarter = turn % 180 !== 0 && src.width > src.height;
  const c = new OffscreenCanvas(quarter ? h : w, quarter ? w : h);
  const g = c.getContext("2d")!;
  g.translate(c.width / 2, c.height / 2);
  if (quarter || turn === 180) g.rotate(turn * Math.PI / 180);
  g.drawImage(src, -w / 2, -h / 2, w, h);
  return new Uint8Array(await (await encodeJpeg(c)).arrayBuffer());
}

// ---------------------------------------------------------------- ops

const ops: Record<string, (args: any) => unknown> = {
  async init({ kit, liteUrl: url, base, canvas: c }: { kit: KitFile[]; liteUrl: string | null; base: string; canvas: OffscreenCanvas | null }) {
    canvas = c;
    [engine, gpu] = await Promise.all([loadEngine(base), startGpu()]);
    for (const { path, file } of kit) {
      const name = path.slice(path.lastIndexOf("/") + 1);
      if (RAW_RE.test(name) && !raws.has("d:" + name)) raws.set("d:" + name, file);
    }
    const films = openLiteFolder(kit) ?? (url ? await openLiteUrl(url) : null);
    if (!films) throw new Error("no LATENT_LITE packs in the chosen folder");
    unwrap(engine.init(await readPack("firmware.bin")));
    for (const b of await Promise.all(films.sort().map(readPack))) unwrap(engine.addFilm(b));
    return { films: unwrap(engine.films()), files: fileList(), gpu: !!gpu };
  },
  files: fileList,
  async thumb({ file }: { file: string }) {
    const t = unwrap(engine.thumb(await rawBytes(file)));
    const bytes = engine.blob();
    const src = t.jpeg
      ? await createImageBitmap(new Blob([bytes], { type: "image/jpeg" }))
      : await createImageBitmap(new ImageData(Uint8ClampedArray.from({ length: t.width * t.height * 4 }, (_, i) => (i % 4 === 3 ? 255 : bytes[(i - (i >> 2))])), t.width, t.height));
    return shrink(src, t.orientation);
  },
  async upload({ name, data }: { name: string; data: ArrayBuffer }) {
    const safe = name.replace(/^.*[\\/]/, "").replace(/[^A-Za-z0-9._ -]/g, "_");
    if (!RAW_RE.test(safe)) throw new Error(`only ${RAW_LABEL} files are accepted`);
    const bytes = new Uint8Array(data);
    const r = engine.check(bytes);
    if ("error" in r) throw new Error(`${safe}: ${r.error}`);
    const id = "u:" + safe;
    engine.forget(id);
    raws.set(id, bytes);
    return id;
  },
  async render({ query, download }: { query: string; download: boolean }) {
    const t0 = performance.now();
    const p = await withFiles(() => engine.prepare(query));
    const stamp = () => ({ ...p.info, ms: Math.round(performance.now() - t0) });
    const g = await usableGpu(p);
    if (!g) {
      unwrap(engine.developCpu());
      return { jpg: engine.blob(), info: stamp(), name: download ? p.name : null };
    }
    if (download) {
      const frame = await g.develop(p.gpu, engine, EXPORT_ROWS);
      const rgba = await g.read(frame);
      frame.texture.destroy();
      g.releaseLarge();
      unwrap(engine.encode(rgba, frame.width, frame.height));
      return { jpg: engine.blob(), info: stamp(), name: p.name };
    }
    const frame = await g.develop(p.gpu, engine);
    g.commit(frame);
    await g.done();
    return { jpg: null, info: stamp(), name: null };
  },
  show({ previous }: { previous: boolean }) {
    const frame = gpu && (previous ? gpu.previous : gpu.current);
    if (frame) gpu!.show(frame);
    return !!frame;
  },
  lose() { gpu?.destroy(); },
  // ?check=1: the same picture through both paths.
  async check({ query }: { query: string }) {
    const p = await withFiles(() => engine.prepare(query));
    if (!gpu) throw new Error("no GPU to compare");
    const t0 = performance.now();
    const frame = await gpu.develop(p.gpu, engine);
    await gpu.done();
    const gpuMs = performance.now() - t0;
    const rgba = await gpu.read(frame);
    frame.texture.destroy();
    const t1 = performance.now();
    const r = unwrap(engine.compare(rgba));
    return { ...r, gpu_ms: Math.round(gpuMs), cpu_ms: Math.round(performance.now() - t1), width: frame.width, height: frame.height };
  },
};

const EXPORT_ROWS = 512;     // rows per submit: long passes can trip the driver's watchdog

async function startGpu() {
  if (!canvas) return null;
  try { return await Gpu.create(canvas); } catch (e) { console.warn("no WebGPU:", e); return null; }
}

// A lost device gets one new try. Pictures past the device's limits take the CPU path.
async function usableGpu(p: Prepared) {
  if (gpu?.lost) gpu = await startGpu();
  return gpu?.fits(p.gpu) ? gpu : null;
}

// One job at a time (the engine blocks the worker). Renders and uploads go before thumbnails,
// and a preview that a newer preview has replaced is answered with {superseded: true}.
const PRIORITY: Record<string, number> = { init: 0, show: 0, lose: 0, upload: 1, render: 1, check: 1, files: 2, thumb: 3 };
interface Job { id: number; op: string; args: any; ticket?: number }
const queue: Job[] = [];
let latestPreview = 0, running = false;

self.onmessage = ({ data }: MessageEvent<Job>) => {
  const job = { ...data };
  if (job.op === "render" && !job.args.download) job.ticket = ++latestPreview;
  queue.push(job);
  if (!running) run();
};

async function run() {
  running = true;
  while (queue.length) {
    // Let messages that arrived during the last job land first, so stale previews are seen.
    await new Promise((r) => setTimeout(r, 0));
    queue.sort((a, b) => PRIORITY[a.op] - PRIORITY[b.op] || a.id - b.id);
    const job = queue.shift()!;
    if (job.ticket !== undefined && job.ticket !== latestPreview) {
      self.postMessage({ id: job.id, out: { superseded: true } });
      continue;
    }
    try {
      const out = await ops[job.op](job.args || {}) as any;
      const transfer: Transferable[] = [];
      if (out instanceof Uint8Array) transfer.push(out.buffer as ArrayBuffer);
      else if (out?.jpg instanceof Uint8Array) transfer.push(out.jpg.buffer);
      (self as unknown as Worker).postMessage({ id: job.id, out }, transfer);
    } catch (e) {
      self.postMessage({ id: job.id, error: (e as Error).message || String(e) });
    }
  }
  running = false;
}
