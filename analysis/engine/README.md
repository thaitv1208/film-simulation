# Engine: pipeline Rust + LibRaw trong một module WebAssembly

App React trong `analysis/app/` tráng ảnh ngay trong tab:

- **`latent_engine.wasm`** gồm pipeline Rust (`core/`) và LibRaw 0.22.1 (`third_party/LibRaw`, không sửa source). Nó giải mã raw, demosaic, giải nén lite pack, dựng cube, tính map halation, sinh noise và ghi JPEG.
- **WebGPU** chạy phần theo từng pixel, từ matrix tới bảng output (`app/src/engine/develop.wgsl`). Đổi phơi sáng hay bật tắt grain không cần tính lại gì ngoài uniform, nên preview gần như tức thì.
- **Không có WebGPU** thì `Frame::develop` trong Rust làm phần đó và trang hiện JPEG. Đây cũng là bản tham chiếu để kiểm tra shader.

`analysis/tools/*.py` là chuẩn đối chiếu và công cụ build lite pack. Từ đây có hai bản của pipeline: sửa một bước trong Python thì phải sửa `core/` và `develop.wgsl`, rồi chạy lại golden. `tools/latent_lite.py` và `core/src/lite.rs` phải đổi cùng nhau khi đổi định dạng pack.

Trang nhận raw `.CR2 .CR3 .ARW .NEF .NRW .DNG .ORF .RW2 .PEF` (danh sách ở `app/src/lib/kit.ts`), với cảm biến Bayer RGGB mà LibRaw có ColorMatrix. Không nhận RAF/X-Trans. Make/Model/Orientation lấy từ IFD0 nếu file là TIFF, từ hộp `CMT1` nếu là CR3, còn không thì từ make/model/flip của LibRaw (`core/src/meta.rs`, `read_meta`). Màu mới chỉ được kiểm chứng trên 5D, 1D X và A7 III.

Kit `.LTP`/`GRAIN.LTG` và backend HTTP không còn trong bản web. Muốn dùng `.LTP` thì dùng CLI `tools/latent_develop.py`.

## Build

```sh
git submodule update --init analysis/engine/third_party/LibRaw
docker build -t latent-engine-build analysis/engine
docker run --rm -v "$PWD:/src" -w /src/analysis/engine latent-engine-build sh build.sh
```

Kết quả nằm trong `app/public/vendor/latent-engine/` và được commit, nên `npm run build` không cần toolchain này. Không để Vite bundle: `engine.worker.ts` import file đó lúc chạy, và `.js` tìm `.wasm` cạnh nó. Module là ES module nhưng phải giữ đuôi `.js`: có host (ví dụ `free.nf`) không gửi `Content-Type` cho `.mjs`, và trình duyệt từ chối module script không có MIME JavaScript, báo `Failed to fetch dynamically imported module`. Thiếu `Content-Type` cho `.wasm` thì không sao: Emscripten tự chuyển sang tải bằng `ArrayBuffer`.

- rustc 1.97 và emsdk 6.0.6, ghim trong `Dockerfile`. Doc của Rust ghi 3.1.68, nhưng `wasm-opt` của bản đó báo `Unknown option '--enable-bulk-memory-opt'`.
- Phải dùng `wasm32-unknown-emscripten`: `wasm32-unknown-unknown` không có libc++ cho LibRaw. Vì vậy không có `wasm-bindgen`; API là các hàm `extern "C"` trong `wasm/src/lib.rs`, `build.sh` tự xuất mọi `engine_*`, và `app/src/engine/wasm.ts` bọc lại.
- LibRaw cần C++ exceptions, Rust build `panic=abort`. `cpp/libraw_shim.cpp` bắt mọi exception, không để chúng đi qua ranh giới FFI. Cả hai phía đều dùng `-fwasm-exceptions -sWASM_LEGACY_EXCEPTIONS=1`, khớp với std của rustc.
- Đơn luồng, nên không cần `SharedArrayBuffer`, COOP/COEP hay service worker. AHD 12,7 MP mất 2–3 giây.

Docker Desktop trên máy Windows dùng để phát triển từng sập khi giải nén image (`wsl-bootstrap` thoát với `0xc00000fd`), để lại layer hỏng: mọi lệnh trong image báo `exec format error`. Cách xử lý: xoá image đó rồi pull lại.

## Kiểm tra

```sh
uv run --script analysis/tools/latent_golden.py        # sinh analysis/engine/golden/, 1,4 GB, gitignore
cargo test -p latent-core -- --nocapture               # trong analysis/engine
```

Cần 3 file raw ở gốc repo (`IMG_0002.CR2`, `IMGL6133.CR2`, `DSC01209.ARW`) và `app/public/LATENT_LITE/`. Không có `golden/` thì test bỏ qua, kèm một dòng ghi chú.

Mở trang với `?check=1` rồi gọi `await latentCheck("file=u:IMG_0002.CR2&film=P400&scale=4")` trong DevTools để so WebGPU với đường CPU trên cùng một ảnh. `await latentLose()` giả lập mất GPU. `?gpu=off` ép chạy đường CPU.

### Độ chính xác

Đo trên 3 file trên và 20 film, ngày 2026-11-01, RTX 3050 Ti. Trước JPEG:

| So sánh | Kết quả |
|---|---|
| Half-size so với rawpy 0.27.1 | Trùng từng giá trị |
| AHD so với rawpy | 0,00001–0,00003% giá trị khác, tối đa 252/65535 |
| Noise grain | Trùng từng bit, kể cả cỡ đầy đủ |
| Gamut, shaper, tetrahedral, grain, bảng output | Trùng tuyệt đối khi đầu vào là `lin` của Python |
| Ảnh ra, Rust so với Python | 0,00008–0,00059% giá trị khác; ±1, riêng HP5P có 3 giá trị ±2 |
| Ảnh ra, WebGPU so với Rust | 0,0001–0,0005% giá trị khác; ±1, riêng HP5P có 1 giá trị ±2 |

Phần lệch Rust so với Python đến từ một bước: `cam @ m.T` qua BLAS dùng FMA nên làm tròn float32 khác 1 ULP. Đường cong B&W dốc có thể biến ULP đó thành 2 mức. Cube có thể lệch tới vài chục trên thang 16 bit ở vài node vùng bảng output phẳng (đen và trắng đã clip), nhưng ở thang 8 bit chỉ lệch tối đa 1.

JPEG khác byte vì đổi encoder (`jpeg-encoder`, vẫn 4:2:0 và bảng chuẩn), nên không so theo byte.

### Tốc độ

Chrome 156, RTX 3050 Ti, `IMG_0002.CR2` (4368×2912):

| | Trước (Pyodide) | WebGPU | Không GPU |
|---|---|---|---|
| Preview ¼ đầu tiên | 10–24 s | 1,6 s | 1,8 s |
| Đổi phơi sáng, grain… | 2–4 s | 6–24 ms | 0,1–0,25 s, cộng JPEG |
| Đổi film (đã tải pack) | | 0,6 s | |
| Export cỡ đầy đủ | 65–90 s | 2,8 s | 5,2 s |

## Các file

| File | Vai trò |
|---|---|
| `core/src/` | Rust thuần, tương ứng `tools/`: `lite`, `color`, `meta`, `develop`, `grain`, `halation`, `jpeg`, và `studio` (cache, query, info). Một lượt gọi thiếu file thì dừng với `Stop::Need`; trang đưa file vào rồi gọi lại, còn những gì đã giải mã nằm sẵn trong cache. |
| `wasm/src/` | API C của module và `Decoder` trên LibRaw. |
| `cpp/libraw_shim.cpp` | API C của LibRaw cho phía Rust. |
| `core/tests/golden.rs` | So từng tầng với dump của `tools/latent_golden.py`. |
| `../app/src/engine/engine.worker.ts` | Web Worker giữ module và GPU: đọc pack, hàng đợi lệnh, thumbnail. Render và upload được ưu tiên hơn thumbnail; preview đã bị lượt mới thay thế thì bỏ qua. |
| `../app/src/engine/gpu.ts`, `develop.wgsl` | Buffer theo `tag`, compute pass, hiển thị, readback. Mỗi dữ liệu nằm trong storage buffer đúng dạng trong heap, nên upload không cần chuyển đổi. |
| `../app/src/lib/backend/engine.ts` | Phía main thread của worker, cấp `OffscreenCanvas`. |

## Chạy

```sh
cd analysis/app
npm install
npm run dev            # phát triển
npm run build          # ra dist/
npm run preview        # xem thử dist/
```

`dist/` là site tĩnh, dùng đường dẫn tương đối nên chép vào đường dẫn con nào của server tĩnh cũng chạy. WebGPU chỉ có trên `https` hoặc `localhost`; mở qua HTTP thường (ví dụ IP trong LAN) thì trang chạy đường CPU.

`public/LATENT_LITE/` được build kèm, nên trang mở thẳng vào lite pack. Nếu không có thư mục đó, trang yêu cầu chọn một thư mục `LATENT_LITE`. Raw nằm trong thư mục đã chọn được liệt kê luôn. Chrome/Edge nhớ thư mục (File System Access API). Raw kéo thả vào trang chỉ nằm trong bộ nhớ của tab và mất khi reload. Không có gì bị upload.

Trang cần WebGPU với Chrome/Edge 113+, Safari 26+, Firefox 141+ (Windows). Đã thử trên Chrome 156 trên Windows; các trình duyệt khác chưa thử.

## Lite pack qua HTTP

`public/LATENT_LITE/` nằm cạnh trang, trong `dist/` là `LATENT_LITE/`. Trang gọi `LATENT_LITE/index.json`; có file đó thì không cần chọn thư mục. Đưa thêm raw vào bằng cách kéo thả, hoặc chọn một thư mục raw qua nút **Kit…**. `latent_lite_build.py` mặc định ghi vào `public/LATENT_LITE/`.

- Lúc khởi động trang chỉ tải `firmware.bin` và 20 `film.json`, tổng khoảng 460 KB. Từng `paper<i>.bin`, `grain<i>.bin`, `tables.bin`, `curves.bin` chỉ được tải khi một lần render cần đến. Thanh trạng thái hiện `downloading …` với file trên 1 MB.
- File đã tải nằm trong Cache Storage. URL có `?v=<crc>`, nên khi build lại, file cũ tự bị bỏ. Cache không mở được thì trang vẫn chạy, chỉ là tải lại mỗi lần.
- Mọi file đều được so với CRC trong `index.json` trước khi dùng hoặc lưu cache, vì host tĩnh và dev server thường trả `index.html` kèm mã 200 cho đường dẫn không có.
- Để lấy pack từ host khác: `?lite=https://host/duong-dan/`. Host đó phải gửi `Access-Control-Allow-Origin` cho trang.
- Trong Windows, profile Chrome nằm quá sâu có thể làm Cache Storage hỏng do giới hạn độ dài đường dẫn. Khi đó trang rơi vào trường hợp không có cache nói trên.

## Khi đưa lên mạng

Chỉ publish `dist/`. Không publish gốc repo: ở đó có `AUTOEXEC.BIN` chứa khóa, cùng `analysis/derived/` và `analysis/ghidra_*`.

**`dist/` chứa `LATENT_LITE/`** (khoảng 510 MB, xem `analysis/README.md`, mục 6.4). Đây là dữ liệu của Latent Studios đã giải mã, và `firmware.bin` tuy không có khóa nhưng vẫn là bảng trích từ firmware của họ. Upload `dist/` lên host công khai nghĩa là phát hành lại toàn bộ dữ liệu đó, nên chỉ làm khi có quyền phân phối. Muốn bản không kèm dữ liệu thì build khi không có `public/LATENT_LITE/`: trang sẽ hiện ô chọn thư mục, và pack của người dùng không rời khỏi máy họ.

`LATENT_LITE/` nằm trong `.gitignore`, nên CI build từ repo sẽ không có nó; bản kèm pack phải build và upload từ máy có thư mục này.
