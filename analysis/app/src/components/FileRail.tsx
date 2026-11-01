import { useEffect, useRef, useState } from "react";
import { cn } from "cn";
import { RAW_ACCEPT, RAW_FILE, RAW_LABEL } from "@/lib/kit";
import type { Backend, RawFile } from "@/lib/types";

function Thumb({ id, backend }: { id: string; backend: Backend }) {
  const [src, setSrc] = useState<string>();
  useEffect(() => {
    let live = true;
    backend.thumb(id).then((url) => { if (live) setSrc(url); }, () => {});
    return () => { live = false; };
  }, [id, backend]);
  return <img loading="lazy" alt="" src={src} className="block aspect-[3/2] w-full rounded object-contain bg-black" />;
}

interface Props {
  files: RawFile[];
  selected: string;
  backend: Backend;
  onSelect(id: string): void;
  onUpload(list: File[]): void;
}

export function FileRail({ files, selected, backend, onSelect, onUpload }: Props) {
  const picker = useRef<HTMLInputElement>(null);
  const [over, setOver] = useState(false);
  const drop = useRef(onUpload);
  drop.current = onUpload;

  useEffect(() => {
    const dragover = (e: DragEvent) => { e.preventDefault(); setOver(true); };
    const dragleave = () => setOver(false);
    const ondrop = (e: DragEvent) => {
      e.preventDefault(); setOver(false);
      drop.current([...(e.dataTransfer?.files ?? [])].filter((f) => RAW_FILE.test(f.name)));
    };
    document.body.addEventListener("dragover", dragover);
    document.body.addEventListener("dragleave", dragleave);
    document.body.addEventListener("drop", ondrop);
    return () => {
      document.body.removeEventListener("dragover", dragover);
      document.body.removeEventListener("dragleave", dragleave);
      document.body.removeEventListener("drop", ondrop);
    };
  }, []);

  return (
    <aside className="overflow-y-auto border-r bg-card p-2.5 [grid-area:files] max-[900px]:flex max-[900px]:gap-2 max-[900px]:overflow-x-auto max-[900px]:border-r-0 max-[900px]:border-b max-[900px]:px-4 max-[900px]:py-2">
      <button
        type="button"
        onClick={() => picker.current?.click()}
        className={cn(
          "mb-2.5 block w-full cursor-pointer rounded-lg border border-dashed px-2 py-3 text-center text-xs text-muted-foreground max-[900px]:mb-0 max-[900px]:flex-[0_0_110px]",
          over && "border-primary text-foreground",
        )}
      >
        Drop raws here<br />or click to add
        <span className="mt-1 block text-[10px] leading-snug opacity-75">{RAW_LABEL}</span>
      </button>
      <input
        ref={picker} type="file" accept={RAW_ACCEPT} multiple hidden
        onChange={(e) => onUpload([...(e.target.files ?? [])])}
      />
      {files.map((f) => (
        <button
          key={f.id}
          type="button"
          onClick={() => onSelect(f.id)}
          className={cn(
            "mb-1.5 block w-full cursor-pointer rounded-lg border border-transparent p-1.5 text-left hover:bg-secondary max-[900px]:mb-0 max-[900px]:flex-[0_0_110px]",
            f.id === selected && "border-primary bg-secondary",
          )}
        >
          <Thumb id={f.id} backend={backend} />
          <span className={cn("mt-1 block truncate text-xs text-muted-foreground", f.id === selected && "text-foreground")}>
            {f.name + (f.uploaded ? " ↑" : "")}
          </span>
        </button>
      ))}
    </aside>
  );
}
