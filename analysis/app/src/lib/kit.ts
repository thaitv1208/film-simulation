import type { DirectoryHandle, KitFile } from "@/lib/types";

// Raws the page takes: Bayer sensors LibRaw reads. Colour is checked only on the 5D, 1D X and A7 III.
export const RAW_EXTS = ["cr2", "cr3", "arw", "nef", "nrw", "dng", "orf", "rw2", "pef"];
export const RAW_FILE = new RegExp(`\\.(${RAW_EXTS.join("|")})$`, "i");
export const RAW_LABEL = RAW_EXTS.map((e) => "." + e.toUpperCase()).join(" ");
export const RAW_ACCEPT = RAW_EXTS.flatMap((e) => ["." + e, "." + e.toUpperCase()]).join(",");
export const KIT_FILE = new RegExp(
  `(^|\\/)([^/]+\\.(${RAW_EXTS.join("|")})|firmware\\.bin|film\\.json|traits\\.json|tables\\.bin|curves\\.bin|(paper|grain)\\d+\\.bin)$`, "i");

export async function walk(dir: DirectoryHandle, prefix = "", depth = 0, out: KitFile[] = []) {
  for await (const [name, h] of dir.entries()) {
    if (name.startsWith(".")) continue;
    if (h.kind === "directory") { if (depth < 5) await walk(h as DirectoryHandle, prefix + name + "/", depth + 1, out); }
    else if (KIT_FILE.test(name)) out.push({ path: prefix + name, file: await (h as FileSystemFileHandle).getFile() });
  }
  return out;
}

// webkitRelativePath starts with the picked folder's own name.
export function fromInput(files: File[]) {
  const list = files.map((file) => ({ path: file.webkitRelativePath.replace(/^[^/]*\//, ""), file }))
    .filter((k) => KIT_FILE.test(k.path));
  return { list, top: files.length ? files[0].webkitRelativePath.split("/")[0] : "" };
}
