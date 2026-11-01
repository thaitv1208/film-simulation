# Phân tích AUTOEXEC.BIN (Latent Studios "Ritual", Canon EOS 5D)

Ghi chép reverse engineering của `../AUTOEXEC.BIN` và các file dữ liệu trong `../LATENT/`.
Toàn bộ là **phân tích tĩnh**: không chạy code trên máy ảnh. Mỗi mục đều ghi rõ cái gì đã kiểm chứng (✔) và cái gì là suy luận (≈).

- File gốc: `AUTOEXEC.BIN`, 209.504 byte, SHA-256 `f0a4a09340592f942fa56822f11672423d4124103f824236ff547067b0e7834d`
- Công cụ: Ghidra 12.2 headless (`/Users/lanho/ghidra_12.2_PUBLIC`, đã build decompiler native mac_arm_64 bằng `gradle buildNatives`), REA (entropy), Python 3.

## 1. Thư mục này có gì

| Đường dẫn | Nội dung |
|---|---|
| `derived/payload_982000.bin` | Payload tách từ `AUTOEXEC.BIN[0x7E0:]`, dài `0x32A80` byte, nạp ở `0x982000`. CRC32 = `0xF7E73EF5`, khớp giá trị loader kiểm tra ✔ |
| `ghidra_out/decompiled_ro.c` | Pseudocode của cả 429 hàm, có chuỗi và hằng số đã được giải. Vùng code được đặt read-only, chỉ chừa ô `0x983F00` ghi được |
| `ghidra_out/functions.tsv` | Danh sách hàm: entry, tên, kích thước, số caller và callee |
| `ghidra_out/strings_xrefs.txt` | Chuỗi và các hàm tham chiếu tới nó |
| `ghidra_out/func_strings.txt` | Các chuỗi literal mà mỗi hàm dùng |
| `ghidra_out/loader_800000.txt` | Disassembly loader (địa chỉ `0x800000`) |
| `ghidra_out/hook_982100.txt` | Disassembly hook tạo task |
| `ghidra_out/chacha20_block.txt` | Disassembly hàm ChaCha20 `FUN_0098912c` |
| `ghidra_project/latent.gpr` | Project Ghidra, mở được bằng GUI |
| `ghidra_scripts/*.java` | Script headless để tạo lại các kết quả trên |
| `tools/latent_crypt.py` | Giải mã `.LTP` và `GRAIN.LTG` |
| `tools/latent_ltp.py` | Đọc `LTPACK2`, giải nén cube `LTZ1`, export `.cube` |
| `tools/latent_color.py` | Tái tạo ma trận camera → đầu vào cube từ giá trị WB của CR2 |
| `tools/latent_develop.py` | Tráng CR2 của 5D (và raw máy khác như Sony `.ARW`, mục 6.1) thành JPG với profile film (P400, V050…) |
| `tools/latent_effects.py` | Grain (nhiễu tất định + lưới 17³ / bảng 8 bit) và halation |
| `tools/latent_traits.py` | Đo tính chất từng film từ cube (tương phản, shadow, highlight, độ bão hòa, ám màu) và sinh mô tả (mục 6.3) |
| `app/src/`, `app/public/` | Giao diện web (React, Vite, shadcn/ui) để xem trước và export, chạy hẳn trong trình duyệt (mục 6.2) |
| `engine/` | Pipeline Rust + LibRaw thành một module WebAssembly, phần theo từng pixel chạy trên WebGPU (mục 6.2) |
| `samples/` | Cube mẫu: P400, 5000 K, push 0, giấy #0 |

### Tạo lại kết quả Ghidra

```sh
G=/Users/lanho/ghidra_12.2_PUBLIC/support/analyzeHeadless
dd if=AUTOEXEC.BIN of=payload.bin bs=1 skip=$((0x7e0)) count=$((0x32a80))
$G proj latent -import payload.bin -processor ARM:LE:32:v5t -loader BinaryLoader \
   -loader-baseAddr 0x982000 -scriptPath analysis/ghidra_scripts \
   -postScript ExportAll.java 0x9a6bc0 out -postScript Refine.java 0x9b4a80 0x9fd0a0 out 0x9a6bc0
# Decompile trong một process riêng để decompiler thấy vùng read-only:
$G proj latent -process payload.bin -noanalysis -readOnly -scriptPath analysis/ghidra_scripts \
   -postScript Decomp.java out/decompiled_ro.c
```

## 2. Cấu trúc AUTOEXEC.BIN

| Offset file | Địa chỉ chạy | Nội dung |
|---|---|---|
| `0x0000`–`0x07DF` | `0x800000` | Loader và các bảng (CRC, relocation, patch) |
| `0x07E0`–`0x2539F` | `0x982000`–`0x9A6BBF` | Code ARMv5TE của payload (khoảng 150 KB) |
| `0x253AE`–`~0x27300` | `0x9A6BCE`… | Chuỗi: tên task, đường dẫn, khóa log, chữ UI, tên film và giấy |
| `0x2731C` | `0x9A8B3C` | Magic `LTCUBE2`, `LTPACK2`, `LTGPACK1`, tiếp theo là 2×32 byte khóa ChaCha20 (đã XOR) |
| `0x27CBE` | | Bảng Huffman JPEG chuẩn |
| `0x29000`–`0x30000` | | Bảng dữ liệu, entropy ≥ 7,2 |
| `0x33100`–`0x3325F` | `0x9B4920`… | `.data` |
| *(không có trong file)* | `0x9B4A80`–`0x9FD0A0` | BSS, được xóa về 0 khi khởi động |

### 2.1 Loader (✔ disassembly đầy đủ)

1. Chuyển sang chế độ SVC, tắt IRQ/FIQ, bật LED (`0xC02200A0 = 0x46`). Clean và invalidate D-cache, drain write buffer.
2. Tính **CRC32 của 20 vùng ROM firmware** (bảng ở `0x648`: addr, len, crc), ví dụ `0xFF810000`/`0x1254`, `0xFFB18B40`/`0x1D8`… Chỉ cần một vùng sai là loader nhảy về `0xFF810000`, tức máy khởi động bình thường. Như vậy payload chỉ chạy trên đúng bản firmware nó nhắm tới (≈ 5D 1.1.1, khớp với `LATBOOT1.FIR`: model ID `0x80000213`, version `1.1.1`).
3. Kiểm tra CRC32 của payload (`0xF7E73EF5`), rồi copy payload tới `0x982000`.
4. Copy `0x1254` byte mã init từ ROM `0xFF810000` sang RAM `0x980000`. Với mọi lệnh B/BL nằm ngoài 10 vùng loại trừ, cộng thêm `0xBA4000` vào trường offset để chúng vẫn trỏ về ROM.
5. Ghi 7 patch (offset, giá trị) vào bản copy. Hai patch đáng chú ý:
   - `+0x11D4`: `BL 0x982100`, thay lời gọi hàm tạo task bằng hook.
   - `+0x10BC`: `mov r1, #0x980000`, ≈ dời giới hạn heap để chừa chỗ cho payload.
6. Dọn cache, tắt LED, nhảy tới `0x982000`.

### 2.2 Payload: khởi động và các task

- `0x982000`: xóa BSS, rồi `mov r0,#2; mov pc,#0x980000`, tức chạy init của Canon đã được patch.
- `0x982100` (hook): bọc `0xFFB18B40` (≈ CreateTask(name, prio, stack, entry, arg)). Nếu entry là `0xFF81138C` thì thay bằng `0x99A944`. Hàm `0x99A944` gọi init gốc `0xFF81138C` trước, sau đó `FUN_009a3590` tạo task **`Latent`** (độ ưu tiên `0x11`, stack `0x2000`, entry `FUN_0099e950`).
- `FUN_0099e950` (task Latent):
  - Chờ tối đa 10 giây cho firmware sẵn sàng (`FUN_009a35e0` đọc các biến Canon ở `0x4AB8`, `0x4ABC`, `0xF3E0`, `0x1AAA4`, `0xF3D4`, `0x12720`, `0x4AC0`, `0x4AC1`). Trong lúc chờ thì nhấp nháy LED `0xC02200F0`.
  - Đăng ký các hàm xử lý GUI và tạo `LatentGui`.
  - Tạo **`LatentDev`** (`FUN_009a1714`, prio `0x1f`, stack `0x4000`: session, thẻ nhớ, roll, tráng ảnh) và **`LatentPic`** (`FUN_0099f768`, prio `0x1a`).
  - Sau đó chạy vòng giám sát mỗi 250 ms.
  - Nếu firmware không sẵn sàng: ghi `A:/LATENT1.LOG`, nháy LED 5 lần rồi dừng.
- Task đọc trước CR2: `LatentRead` (`FUN_00985158`), dùng semaphore `LatentAheadWork` và `LatentAheadDone`.

### 2.3 Hàm firmware được gọi (≈ đặt tên theo cách dùng)

| Địa chỉ | Tên đoán | Căn cứ |
|---|---|---|
| `0xFFB18B40` | CreateTask | `("LatentRead", 0x1e, 0x1000, fn, 0)` |
| `0xFFB17EA0` | CreateBinarySemaphore | `("LatentAheadWork", 0)` |
| `0xFFB23474` | msleep | `(1000)`, `(100)`, `(0xfa)` |
| `0xFFB1828C` / `0xFFB18430` | TakeSemaphore(timeout) / GiveSemaphore | |
| `0xFFB5610C` | tìm task theo tên | `("tGuiMainTa")` |
| `0xFF93A608` / `0xFF93AA58` | bắt đầu / kết thúc mã hóa JPEG phần cứng | `FUN_0098965c` |
| `0xFFB18F00` | ExitTask | dùng ở cuối luồng lỗi |

Tổng cộng 40 địa chỉ ROM. Muốn đặt tên chính xác cần dump ROM 5D 1.1.1.

## 3. Mã hóa file `.LTP` / `GRAIN.LTG` (✔ đã giải mã được)

- `FUN_00983d0c`: mở file và đọc 8 byte đầu. Nếu 8 byte này đã là magic (`LTPACK2\0` hoặc `LTGPACK1`) thì file không mã hóa. Nếu chưa, giải mã rồi so lại.
- `FUN_0098943c`: tạo nonce gồm `{nhãn 4 byte, 4 ký tự đầu của đuôi đường dẫn, 0x00004C54}`:
  - `.LTP`: nhãn `LTPK`, đuôi 8 ký tự. Ví dụ `P400.LTP` → nonce `LTPK` `P400` `TL\0\0`.
  - `GRAIN.LTG`: nhãn `LTGR`, đuôi 14 ký tự. Ví dụ `P400/GRAIN.LTG` → `LTGR` `P400` `TL\0\0`.
- `FUN_0098912c`: **ChaCha20** chuẩn (hằng `expand 32-byte k`, 20 vòng, bố cục IETF: counter 32 bit ở word 12, nonce 96 bit).
- `FUN_0098948c`: XOR keystream bắt đầu tại offset bất kỳ của file (counter = `offset >> 6`), nên đọc ngẫu nhiên được.
- **Khóa 256 bit** = `payload[0x9A8B54..+32] XOR payload[0x9A8B74..+32]`. Script đọc khóa trực tiếp từ `AUTOEXEC.BIN`, không lưu khóa trong repo.

File `.STP` (ví dụ `P400/S02.STP`) có header rõ `LTS1`, phần sau có entropy 8.0. ≈ Đây là bộ đệm các "bước" cube đã giải mã (khóa log `table_steps_stored`). Chưa phân tích.

## 4. Định dạng `LTPACK2` (nội dung `.LTP` sau giải mã) — ✔ checksum index khớp trên cả 20 film

```
0x00  "LTPACK2\0"
0x08  u32 index_end          hết header + index
0x0C  u32 65 | 0x10 u32 8000 | 0x14 u32 31250    (hằng số firmware bắt buộc)
0x18  char[8] mã film        "P400"
0x20  u16 n_light, n_push, n_grade, n_extra
0x28  u32 n_cubes = n_light*n_push*n_grade
0x2C  u32 n_extra_words = n_extra*n_grade*n_push
0x30  u32 sum, 0x34 u32 sum_of_sums   (Fletcher trên các u32 từ 0x40 tới index_end)
0x38  u32 offset của khối bảng | 0x3C u32 kích thước khối bảng (= 0x13002)
0x40  i32[n_light]   trục ánh sáng, mired dạng Q16 (100..400 mired = 10000K..2500K)
      i8[n_push]     trục push/pull (−6..+6; B&W chỉ có 0..+6)
      u8[n_grade]    trục giấy/phim in; bit 7 = giấy có thêm trục grade
      u8[n_extra]    các mức grade (0,2..12) cho giấy có bit 7
      (u32 off, u32 size)[n_cubes]   mỗi mục trỏ tới một blob LTZ1
      u32[n_extra_words]
...   các blob LTZ1 (mỗi blob khoảng 200 KB)
off(0x38): khối bảng u16[0x9801]
```

**Thứ tự cube** (theo `FUN_0098533c`): `index = (grade_i * n_push + push_i) * n_light + light_i`. Ánh sáng thay đổi nhanh nhất. ✔ Firmware nạp cả dãy `n_light` mục index của một cặp (giấy, push); ≈ sau đó nội suy giữa hai mức mired kề nhau theo cân bằng trắng (khóa log `light_mired_q16`, `light_steps`).

**Ý nghĩa trục giấy** (đối chiếu số lượng với danh sách chuỗi):

| Loại film | n_grade | Ánh xạ ≈ |
|---|---|---|
| Âm bản màu (P160/P400/P800, EK10, G200, U400, S400, H400) | 10 | Endura Premier, Ektacolor Edge, Endura Premier Metallic, Portra Endura, Supra Endura, Ultra Endura, Endura Transparency, Crystal Archive Supreme, Crystal Archive Type II, Agfacolor Type 12 |
| Điện ảnh (D250, T500) | 4 | Kodak 2383, Kodak 2393, Agfa CP30, Fuji ETERNA-CP |
| Dương bản (E100, F100, V050, K064) | 2 | Light box, Ektachrome Radiance III |
| B&W (HP5P, TM40, X400, AC10, DL32) | 3, có cờ `0x80` | Ilford Multigrade IV RC, FB Classic, FB Warmtone; trục `extra` là grade giấy |
| PBW4 | 10 + 3 cờ | in được trên cả 10 giấy màu lẫn 3 giấy B&W |

Với giấy có cờ `0x80`, đoạn decompile cho thấy firmware dùng dãy cube của giấy có cờ đầu tiên với push index 0, rồi đọc thêm 1 u32 từ `extra_words` tại `(grade_i*n_push + push_i)*n_extra + extra_i`. ≈ Phần chọn cube này chưa kiểm chứng.

✔ Giá trị u32 đó là **offset tới một khối `LTVC`** dài `0x3004` byte trong `.LTP` (`FUN_009865ac`): `"LTVC"`, rồi 3 bảng 4096 byte cho R, G, B. Ba bảng này thay cho bảng xuất 8 bit mặc định (mục 4.2), tức chúng là đường cong giấy/grade của film B&W. Khi không có `LTVC`, cả 3 kênh dùng chung bảng mặc định.

### 4.1 Blob `LTZ1` (cube 3D nén) — ✔ giải mã được, checksum khớp (đã thử K064 #0, P400 #63 và #650)

```
0x00 "LTZ1"  0x04 u8 N (=65)  0x05 u8 flags  0x06 u8 10  0x07 u8 10
0x08 u32 sum, 0x0C u32 sum_of_sums của các word đã giải mã
0x10 u32 độ dài stream
0x14 3 kênh x 10 context: u8 k, u16 freq[k] (tổng = 1024)
     u32 trạng thái rANS khởi đầu, sau đó là các u16 little-endian
```

- Kết quả giải mã là N lát × N hàng × N cột word u32: `c0 | c1<<11 | c2<<22` (11/11/10 bit). Chỉ số = `z*N*N + y*N + x`.
- ✔ Theo dữ liệu, trục x ứng với R, y với G, z với B. Thứ tự này trùng với chuẩn `.cube` (R chạy nhanh nhất).
- Dự đoán:
  - Lát đầu: planar 2D `left + up − upleft`. Nếu bit cờ của kênh bật thì dùng `(2·left + 2·up − upleft)/3`.
  - Các lát sau: nếu bit cờ bật, `(2(up+left+P) − upleft − P_left − P_up)/3`, với P là voxel cùng vị trí ở lát trước.
- Phần dư: rANS 10 bit, nạp lại 16 bit, ký hiệu 40 là escape `(e: 4 bit, sau đó e bit) + 2^e + 39`, giải theo zigzag. Context = tổng |phần dư| của 4 ô lân cận (trái, trên-trái, trên, trên-phải), quy về 10 nhóm theo các ngưỡng `1,2,3,5,7,10,15,22,34`.

### 4.2 Khối bảng (offset ở header 0x38, kích thước 0x3C)

| Chỉ số u16 | Vai trò | Căn cứ |
|---|---|---|
| `[0..0x8000]` | **Shaper**: giá trị tuyến tính Q16 (bước 16, dải 0..8.0) → tọa độ lưới ×512. Ví dụ 0.18 ≈ ô 23, 1.0 ≈ ô 40, 8.0 = ô 64 | ✔ dùng trong `FUN_0098c00c` |
| `[0x8001..0x8800]` | Bảng 4096 byte: chỉ số 12 bit → 8 bit, đường cong kiểu sRGB (1/16 → 71) | ✔ `param[0x93..0x95]` |
| `[0x8801..0x8C00]` | Shaper mịn cho vùng −0.125..0.125 | ≈ `FUN_00994150` |
| `[0x8C01..]`, `[0x9001..]`, `[0x9401..]` | 3 đường cong **nén gamut** theo kênh (1024 mục, x = khoảng cách/16 theo Q12, gần đồng nhất ở đoạn đầu, bão hòa ở 1.023 / 1.148 / 1.047) | ✔ `FUN_00991240` |

### 4.3 Các file khác trong `LATENT/`

- `GRAIN.LTG`: mã hóa giống `.LTP` (nhãn `LTGR`). Sau giải mã: `LTGPACK1`, u32 n_grade (10), u32 n_push (13), trục giấy, trục push, rồi các khối `LTGRAIN4`, mỗi khối `0xFE58` byte, nằm tại `0x30 + (grade_i*n_push + push_i)*0xFE58` (`FUN_009865ac`). Cấu trúc khối (`FUN_00994378`): `"LTGRAIN4"`, byte cờ ở `+8`:
  - bit1 = 0: bảng tiền xử lý 3 kênh ở `+0xC` (`0x17F4` byte, giá trị ≤ 0x7FFF), rồi một lưới **17³ × 12 byte** (6 giá trị int16 mỗi nút) = biên độ grain phụ thuộc màu.
  - bit1 = 1 (bit0 bắt buộc): bảng ở `+0xC` và 12 mức × 3×256 byte biên độ ở `+0xF0C`, chọn mức bằng `FUN_00994488` (mức = grade giấy, mặc định 5).
- `FILM.A/B`, `FILMS.A/B`, `ROLL.A/B`, `OWED.A/B`, `PART.A/B`: container `LTRC`: `"LTRC", u32 loại, u32 độ dài, u32 (≈ checksum)`, tiếp theo là payload (`LTF3`, `LTR4`, `LTO2`…). Mỗi trạng thái ghi luân phiên vào 2 bản A/B để an toàn khi mất điện.
- `BODY.ID` = `Ritual 5D`, `CARD.MRK` = `marked`, `LAST.TXT` = số frame cuối, `SESSION.TXT` = số session.
- `*.LOG`, `P000n.LOG`, `R000n.LOG`: log văn bản dạng khóa=giá trị (xem chuỗi `pass_picture_jpeg`, `develop_stage`…). `1000001.YUV` (259.200 byte) ≈ ảnh xem trước dạng YUV.

## 5. Pipeline CR2 → JPEG

1. **Lấy file raw.** Ưu tiên đọc CR2 còn nằm trong RAM của Canon: `FUN_0099b260` kiểm tra `II*\0` và `CR` trong bộ đệm uncached `0x10000000|addr` (khóa log `memory_reused`, `pictures_from_memory`). Nếu không có thì đọc từ `A:/DCIM/xxxEOS5D/` (`FUN_0099d198`), với task `LatentRead` đọc trước theo khối 64 KB (`cr2_reads`, `cr2_reads_ahead`).
2. **Phân tích TIFF/CR2:** `FUN_0098f788` kiểm tra header `II*`, tìm IFD raw. Ảnh JPEG nhúng của Canon được tìm bằng các marker `FF D8` (`FUN_0099b1d4`).
3. **Giải mã raw lossless JPEG:** `FUN_009906b4` → `FUN_0098d858`, bộ giải mã Huffman có xử lý byte nhồi `FF 00` và các bảng `0xC48`/`0xC8C` của CR2.
4. **Từ raw sang RGB tuyến tính** (`FUN_0098c00c` cho bản nhỏ, `FUN_009928e8` cho bản đầy đủ):
   1. Trừ mức đen, nhân gain Q12, cắt theo mức trắng của từng kênh.
   2. Demosaic Bayer bằng cách cộng 2×2 có trọng số 1-3-3-9 (≈ giảm độ phân giải theo "Share size").
   3. Nhân ma trận màu 3×3 (Q12). Ma trận gộp cả cân bằng trắng và chuyển từ camera sang Rec.709 tuyến tính. Chi tiết ở mục 5.1.
   4. Cộng offset halation theo từng pixel (chỉ với film điện ảnh, xem mục 5.3).
   5. **Nén gamut** (`FUN_00994c68` → `FUN_00991240`). Với mỗi kênh c, gọi max là kênh lớn nhất. Tính d = (max − c)/max. Nếu d vượt ngưỡng 0.815 / 0.803 / 0.880 (R/G/B, đúng bằng ngưỡng mặc định của ACES Reference Gamut Compression) thì thay c = max·(1 − curve_c(d)), với curve_c là bảng của film (mục 4.2). Nếu max ≥ 8.0 thì luôn nén.
5. **Film:**
   1. Qua shaper (mục 4.2) để đổi giá trị sang tọa độ lưới.
   2. **Nội suy tứ diện** trong cube 65³. Cube này là kết quả nội suy theo mired từ hai cube kề nhau, đúng (film, push, giấy) đang chọn.
   3. Mở rộng output lên 16 bit. Phần vượt 8.0 của đầu vào (ngoài dải shaper) được cộng thẳng vào output.
6. **Grain** (`GRAIN.LTG`, khối `LTGRAIN4` theo (giấy, push)), chi tiết ở mục 5.2.
7. **Ra 8 bit:** bảng 4096 byte (mục 4.2). Với đường phần mềm, chuyển RGB sang YCbCr BT.601 (`0.257R+0.504G+0.098B+16`…).
8. **JPEG:** dùng bộ mã hóa phần cứng của Canon qua `0xFF93A608`/`0xFF93AA58` (`FUN_0098965c`, khóa log `hw_frame_encode_convert_ms`). Payload tự ghép SOI, APP1 EXIF (`FUN_00996940`: Make `Canon`, Model, Software `Ritual`, ImageDescription) với dòng dữ liệu phần cứng. Khi cần, đường phần mềm (`FUN_0098a170`) dùng bảng Huffman chuẩn có sẵn trong payload. Ảnh ghi vào `A:/RITUAL/xxxEOS5D/`.

### 5.1 Không gian màu đầu vào của cube (✔ đọc từ `FUN_009944b0`, tái tạo trong `tools/latent_color.py`)

**Kết luận: đầu vào cube là RGB tuyến tính, primaries Rec.709/sRGB, điểm trắng D65, đã cân bằng trắng, scene-referred. Q16: 1.0 = `0x10000`, dải shaper 0..8 (và −0.125..0.125 ở vùng mịn).**

Cách firmware dựng ma trận (toàn bộ tính bằng double soft-float: `9a5e5c`=dadd, `9a5e58`=dsub, `9a6274`=dmul, `9a6504`=ddiv, `9a6190`=i2d, `9a616c`=u2d, `9a6854`=d2i):

1. **Bảng camera** tại `0x9A9080`:
   - con trỏ tới tên `"Canon EOS 5D"`;
   - ColorMatrix XYZ→camera `[0.6347 −0.0479 −0.0972; −0.8297 1.5954 0.248; −0.1968 0.2131 0.7649]`, trùng với Adobe/dcraw cho 5D;
   - mức đen 127, mức trắng 3692;
   - mẫu CFA `0xB4B4B4B4` và các offset viền 90/34;
   - BaselineExposure `+0.25`.
   Firmware so tên model trong CR2 với tên này; khác là từ chối (lỗi `−11`).
2. **WB:** đọc `WB_RGGBLevelsAsShot` của CR2 (u16 ở `+0x38` R, `+0x3A` G, `+0x3E` B). Vector trung tính của camera là `n = (G/R, 1, G/B)`.
3. Nghịch đảo ColorMatrix (`FUN_00993bd4`). Điểm trắng cảnh = `Minv·n`, chuẩn hóa về Y = 1 (`FUN_00992700`).
4. **Bradford** (ma trận ở `0x9A8CD0`, `FUN_00993fe0`): điểm trắng cảnh → D50 `(0.96430, 1, 0.82510)`, rồi D50 → D65 `(0.95046, 1, 1.08906)`.
5. Nhân với **XYZ(D65) → sRGB tuyến tính** ở `0x9A8D18` `[3.240454 −1.537138 −0.498531; …]` (số của Lindbloom).
6. Nhân hệ số phơi sáng **`2^(BaselineExposure + 0.943)` = 2^1.193 ≈ 2.286**. Hằng 0.943 là của Latent tự thêm.
7. Làm tròn về Q12, lưu ở offset `0x60..0x80` của develop context. `FUN_009927bc` copy sang `FUN_0098c00c`.
8. Raw trước khi nhân ma trận được chuẩn hóa: `gain = 2^28/(white − black)` (Q12, offset `0x84`), nên mức bão hòa cảm biến = 1.0. Từng kênh bị cắt ở `wb_i / max(wb) · 1.0` để highlight trung tính cắt đều cả 3 kênh.

**Mức xám** (≈ suy ra từ phép tính, chưa đối chiếu với CR2 thật):
- Một màu trung tính vừa làm bão hòa photosite G cho ra giá trị tuyến tính khoảng **2.35** (WB ánh sáng ngày) đến **2.9** (WB đèn sợi đốt). Như vậy 0.18 nằm khoảng 3.7–4.0 stop dưới điểm cắt.
- Đây là khoảng cách quen thuộc giữa mức xám đo sáng và điểm bão hòa của Canon. Vì vậy, ≈ một cảnh đo sáng đúng sẽ có mức xám xấp xỉ **0.18**, và hằng 0.943 có lẽ được chọn để đạt điều này.
- Trên shaper, 0.18 tương ứng ô lưới khoảng 23/64.

**Ánh sáng (trục mired):**
- `FUN_0099a0f8` lấy log2 của R/G và B/G từ cùng các mức WB, tra bảng 2D 80×88 (`FUN_0099a064`) ra sắc độ, rồi dò trên 30 điểm của quỹ tích Planck ở `0x9A97B0` (kiểu phương pháp Robertson) để ra mired.
- Sau đó `FUN_00983430` chọn hai cube kề nhau và trọng số. Ví dụ log thật trên thẻ: `light_mired_q16 = 0x00AFAA05` (175.66 mired, `light_kelvin_outside = 5693`), `light_steps = 0x070302`, tức trên P400 nội suy giữa index 2 (156) và 3 (200) với trọng số 7/16 ≈ (175.66−156)/44.
- Nghĩa là ma trận đã đưa ảnh về D65, còn **trục ánh sáng của cube mô phỏng lại phản ứng của film với nguồn sáng**: film cân bằng ánh sáng ngày chụp dưới đèn sợi đốt sẽ ngả cam.

### 5.2 Grain (✔ `FUN_009912ec`, `FUN_00994378`, `FUN_009928e8`)

**Nguồn nhiễu: tự sinh, tất định theo tên file.**
- Seed = **FNV-1a 32 bit của tên file CR2**, OR với 1 (`FUN_009865ac`, lưu ở `ctx+0x17C2C`). Cùng một frame tráng lại bao nhiêu lần cũng ra đúng hạt đó.
- Mỗi hàng ảnh (`FUN_009912ec(ctx, width, row, n)`) và mỗi luồng k (1 luồng nếu grain mono, 3 nếu grain màu):
  1. `h = row·0x9E3779B9 ^ seed ^ k·0x2545F491`, qua bước trộn `fmix32` của MurmurHash3, rồi một bước xorshift32 (13/17/5). Kết quả là trạng thái đầu hàng.
  2. Mỗi lần xorshift32 cho 4 byte; mỗi byte tra bảng `0x9A8F7C`: 256 giá trị int8 đã sắp xếp từ −21 đến +21, độ lệch chuẩn 7.1. Đây là **hàm ngược CDF của phân bố Gauss**, nên mỗi byte ngẫu nhiên thành một mẫu Gauss.
  3. Lọc **[1, 4, 1] theo chiều ngang** (hai mẫu trước + 4·mẫu giữa + mẫu hiện tại) và **[1, 4, 1] theo chiều dọc** (giữ 2 hàng trước trong bộ đệm luân phiên `ctx+0x17C32`). Kết quả là nhiễu int16 tương quan khoảng 3×3 pixel, tức "hạt" có kích thước chứ không phải nhiễu từng điểm.
  4. Ghi vào `ctx+0x1AF78` (luồng 0 và 1, xen kẽ) và `ctx+0x1D1A8` (luồng 2).
- Số luồng: 1 nếu cờ bit0 của `LTGRAIN4` bật (grain mono), 3 nếu không.

**Cách cộng vào ảnh** (khối `LTGRAIN4` của (giấy, push)):
- **Lưới 17³ (cờ bit1 = 0):**
  - 3 bảng tiền xử lý (1024 u16 mỗi kênh, ở `+0xC`) đổi output 16 bit của cube thành (ô lưới 0..15, phần lẻ 11 bit). Giá trị < 1024 dùng `v/2`, còn lại dùng `(v>>7)+504`, tức bước mịn hơn ở vùng tối.
  - Mỗi nút lưới có 6 int16 tạo thành **ma trận tam giác dưới** L = `[[a,0,0],[b,c,0],[d,e,f]]`, nội suy tứ diện, rồi `[ΔR,ΔG,ΔB] = (L · [n0,n1,n2]) >> 11 >> 7`, cộng vào giá trị 16 bit trước bảng 8 bit.
  - Nghĩa là **ma trận hiệp phương sai của hạt thay đổi theo màu** (L giống phân rã Cholesky). Ở chế độ mono chỉ dùng cột `n0`.
- **Đơn giản (cờ bit1 = 1):** `out8 = clamp(v + (n0·amp[c][v] + 0x400) >> 11)` sau bảng 8 bit. `amp` là 12 mức × 3 kênh × 256 byte ở `+0xF0C`; mức chọn bằng `FUN_00994488`, theo grade giấy (B&W) hoặc mặc định 5.
- Khối `LTGRAIN4` nằm ở `0x30 + (grade_i·n_push + push_i)·0xFE58` trong `GRAIN.LTG`.

### 5.3 Halation (✔ cấu trúc; ≈ một số hệ số)

**Chỉ dành cho film điện ảnh.**
- `FUN_00994d0c` tra mã film trong bảng `0x9A90F0`: 6 bản ghi × 0x34 byte, gồm `D250NX`, `T500IR`, `D050…`, `T200…`, `F250…`, `EV16…` (Vision3 250D/500T/50D/200T, Fuji Eterna 250, Eterna Vivid 160). Film không có trong bảng thì không có halation.
- Trong `LATENT/` hiện chỉ có `D250` và `T500` là có halation. Điều này hợp lý: film điện ảnh có lớp remjet chống halation, bỏ lớp đó đi (kiểu CineStill) thì xuất hiện quầng đỏ.
- Mức halation (`halo_level`) lấy từ cài đặt, tối đa 256.

**Bước 1: thu tín hiệu khi giải mã raw** (`FUN_00994db0` khởi tạo, `FUN_00996450` cộng dồn):
- Ảnh được chia thành lưới **360 × 213 ô**, mỗi ô 2 kênh u32.
- Photosite nào có giá trị ≥ **0.97 × mức trắng** (gần cắt) thì cộng `v · knee((v − ngưỡng)·k) >> 8` vào ô của nó. `knee` ở `0x9A8D68` có 257 mục, tăng dần dạng chữ S: 0, 3, 14, 67, 175, 256.
- Như vậy chỉ **vùng cháy sáng** mới tạo ra halation.
- Hai vector màu (2×3) được tính sẵn bằng cách nhân hệ số trong bản ghi profile với ma trận camera Q12.

**Bước 2: làm mờ** (`FUN_00994f70`, chạy cho 2 bản đồ):
- Giảm 2× (trung bình 2×2) xuống 180×106.
- Làm mờ tách biến bằng nửa kernel ở `0x9A9238` (10 hệ số: 4900, 4567, 3700…) và `0x9A925E`.
- Làm mờ thêm bằng 2 kernel 35 tap ở `0x9A9274`: một Gauss hẹp (…783, 3187, 7398, 9794…) và một Gauss rộng (…3700, 4567, 4900…). Hai kết quả trộn với trọng số **(−7282, 25486)** (`0x9A9418`), tạo ra quầng rộng có viền.
- Phóng lại và ghi đè lên bản đồ.

**Bước 3: áp vào ảnh** (`FUN_00995d5c`, gọi cho từng hàng):
- Nội suy song tuyến từ bản đồ ra hàng ảnh, nhân với vector màu của film, rồi cộng vào RGB tuyến tính ngay sau ma trận màu, trước khi nén gamut.
- Vì RAM không đủ, bản đồ được phân trang thành các khối 32 hàng × 360 ô. **`HALO.A/B` trên thẻ là file swap** của các khối này (`FUN_009842cc` đọc ở offset `row·0xB400 + 0x20`); khóa log `halo_pool_kb` ghi lượng RAM dùng cho phần này.

**Bản ghi profile (0x34 byte):** `char code[4]`, `i32 p0[3]`, `i32 p1[3]` (độ nhạy lớp nhũ tương đỏ/lục theo RGB tuyến tính, Q15), rồi `c0`/`c1` xen kẽ ở `+0x1C..+0x30` (màu quầng, Q16).
- `FUN_00994db0` tính `vec = p · M` (M = ma trận Q12 ở mục 5.1) để quy độ nhạy về không gian camera.
- Trong `FUN_009919e0`, mỗi ô thu (lưới 720 × 213) nhận `plane = vec · (trung bình ô + 7 × trung bình phần vượt ngưỡng)`. Hai ô thu cạnh nhau cộng vào một ô bản đồ (360 cột), giá trị lưu ở Q9.
- Bản đồ cuối (`FUN_00994f70`) = (0x38E4·rất rộng − 7282·hẹp + 25486·rộng) / 32768. Hai kernel hẹp là 11 và 9 tap, hai kernel rộng là 19 và 17 tap; "rất rộng" là kernel rộng chạy trên bản đồ đã giảm 2×.
- Khi áp vào ảnh: `ΔRGB = (strength/256) · (m0·c0 + m1·c1)`, với `strength` = {85, 171, 256}[mức cài đặt 0..2]; mặc định mức 2 (`.data` = 2, khớp `halo_level = 2` trong log).

✔ Hệ quả của cách đọc này: nguồn halation chứa cả độ sáng trung bình, và tổng trọng số của phép làm mờ bằng 1, nên **toàn ảnh bị nâng đỏ** chứ không chỉ quanh vùng cháy. Điều này đã được xác nhận bằng JPEG do máy xuất (`0004D250.JPG`: `IMG_0004.CR2`, D250, giấy 2383 = giấy #0, push 0, Halation Full, Light 5700 K, ảnh half-size): xa nguồn sáng, ảnh máy cao hơn bản không halation khoảng R +19, G +5 trên 255.

**Kênh được thu** (`FUN_00996450`, `FUN_00991d5c`): mỗi ô giữ tổng riêng cho 4 vị trí CFA (hàng chẵn/lẻ × cột chẵn/lẻ), gộp thành R, G, B theo bảng CFA ở `ctx+0x9C` (R = 0, G = 1, B = 2).
- Phần trung bình: mỗi photosite bị cắt ở mức trắng theo WB của kênh đó (`ctx + (màu+0x22)·4`). Vì vậy vùng cháy được thu như màu trung tính, chứ không phải màu đỏ đậm.
- Phần highlight: `FUN_00994db0` đặt ngưỡng của R và B bằng −1, nên chỉ **photosite G** vượt 0.97·clip mới được cộng.

≈ **Đối chiếu với ảnh máy:** làm đúng từng chữ cách đọc trên (highlight chỉ vào kênh G, ×7) cho quầng quanh đèn quá yếu, và plane đỏ ở lõi bị âm vì cột G của `vec0` là −1.97. Vẫn còn một chỗ chưa đọc đúng. Tool hiện dùng mô hình khớp với ảnh máy:
- thu `min(v, clip)`;
- cộng phần highlight của G như một lượng trung tính `80 · g · knee · clip`;
- nhân phần trung bình với 1.35 (`HALO_HIGHLIGHT_GAIN`, `HALO_MEAN_GAIN` trong `latent_effects.py`).

Ở 5700 K, sai lệch trung bình còn R +0.4, G +0.2, B −2.6. Kênh B lệch cả khi tắt halation, nên phần đó không đến từ halation. Tool thu từ ảnh đã demosaic, không thu từ photosite.

## 6. Dùng các tool

```sh
# Giải mã một file (hoặc chỉ phần đầu với --length)
python3 analysis/tools/latent_crypt.py --autoexec AUTOEXEC.BIN LATENT/P400.LTP /tmp/P400.dec --length 0x10000

# Xem các trục của một film pack
python3 analysis/tools/latent_ltp.py --autoexec AUTOEXEC.BIN LATENT/P400.LTP

# Giải nén một cube và export:
#   <prefix>.cube        3D 65³, input = giá trị đã qua shaper (0..1), output = giá trị 8 bit hiển thị / 255
#   <prefix>_shaper.cube 1D, tuyến tính 0..8 → input của cube
python3 analysis/tools/latent_ltp.py --autoexec AUTOEXEC.BIN LATENT/P400.LTP \
    --at 3 6 0 --export analysis/samples/P400_5000K_push0_paper0
#   --at LIGHT PUSH GRADE là chỉ số trên các trục (ở đây: 200 mired = 5000K, push 0, giấy #0)
```

### 6.1 Tráng CR2 → JPG với profile film (`tools/latent_develop.py`)

```sh
# Đầu vào: file .CR2 (Canon EOS 5D) và mã film; đầu ra: <tên raw>.jpg cạnh file raw
uv run analysis/tools/latent_develop.py IMG_0002.CR2 P400
uv run analysis/tools/latent_develop.py IMG_0002.CR2 V050 --paper 1 --push 0 --half
#   --paper N   chỉ số giấy/phim in (mặc định 0; danh sách ở mục 4)
#   --push S    giá trị trên trục push của film (−6..+6, mặc định 0)
#   --grade G   grade giấy B&W 0..11 (mặc định 5; chỉ dùng với giấy có cờ 0x80)
#   --kelvin K  nhiệt độ màu ánh sáng (K), như chế độ Light Kelvin của máy
#               (mặc định: ước lượng từ WB của ảnh bằng công thức McCamy)
#   --no-gamut  bỏ bước nén gamut (để so sánh)
#   --no-grain  bỏ grain
#   --halation L  mức halation 0..2 (85/171/256 trên 256, mặc định 2; chỉ film điện ảnh D250/T500)
#   --no-halation bỏ halation
#   --card-path P đường dẫn mà máy sẽ dùng cho file raw này, quyết định seed của grain
#               (mặc định A:/DCIM/100EOS5D/<TÊN RAW>)
#   --half      xuất nửa kích thước (2184×1456), nhanh hơn
#   -o PATH     đổi tên file đầu ra
```

- Script khai báo phụ thuộc theo PEP 723 (`numpy`, `rawpy`, `pillow`). `uv run` tự cài vào cache của uv.
- LibRaw (rawpy) chỉ được dùng để giải mã lossless JPEG và demosaic (AHD). Các bước còn lại làm theo firmware:
  - mức đen/trắng 127/3692 của bảng camera;
  - cắt từng kênh theo WB;
  - ma trận ở mục 5.1;
  - nén gamut với 3 đường cong của film (mục 5, bước 4.5);
  - shaper, cube 65³ nội suy tứ diện, trộn giữa 2 mức mired;
  - bảng 8 bit của `.LTP`. Với giấy B&W, dùng 3 đường cong `LTVC` chọn theo (giấy, push, grade).
- Kết quả với `IMG_0002.CR2` (WB RGGB 2089/1024/1024/1383): ≈5250 K, P400 trộn cube 62/63, ảnh 4368×2912, khoảng 8 giây.
- Thử nghiệm: với V050, nén gamut đổi khoảng 3,6% pixel (lá cây, hoa đậm màu), lệch tối đa 29/255. Với HP5P, grade 2 → 5 → 9 cho tương phản tăng dần.
- Grain (`tools/latent_effects.py`, mục 5.2): film màu dùng lưới 17³ với 3 luồng nhiễu, film B&W dùng hạt đơn sắc trên bảng 8 bit. Với P400 ở 4368×2912, độ lệch chuẩn của hạt khoảng 2/255 mỗi kênh. Hạt của HP5P rõ hơn nhiều. Firmware tạo nhiễu ở kích thước ảnh đầu ra; ảnh của máy có thể nhỏ hơn ("Share size"), khi đó hạt sẽ trông to hơn.
- Halation (mục 5.3): chỉ D250 và T500. Hai hệ số của bước thu được khớp theo `0004D250.JPG`, không đọc được từ firmware.
- Ánh sáng: máy cho chọn Kelvin trực tiếp. Muốn so với JPEG của máy thì truyền đúng giá trị đó, ví dụ `uv run analysis/tools/latent_develop.py IMG_0004.CR2 D250 --half --kelvin 5700`. Ước lượng tự động của tool cho ảnh này ra khoảng 5250 K.
- Thời gian: khoảng 18 giây cho ảnh đầy đủ có grain (đa phần để sinh nhiễu).
- Chưa tái tạo:
  - vùng giá trị âm của shaper;
  - bảng WB→mired của firmware (tool dùng công thức CCT của McCamy nên mired có thể lệch vài đơn vị).
- **Film B&W** (HP5P, TM40, X400, AC10, DL32 và giấy B&W của PBW4): ≈ cách chọn cube (giấy có cờ đầu tiên, push index 0) đọc từ decompile, chưa đối chiếu với ảnh thật của máy ảnh. Nghĩa của từng mức grade (0..11 ↔ giá trị 0, 2..12 trên trục `extra`) cũng chưa rõ.

#### Raw của máy khác (Sony A7 III `.ARW`, …)

Firmware chỉ có bảng camera cho 5D. Với file không phải CR2, tool lấy từ LibRaw những gì bảng camera chứa:
- ColorMatrix XYZ→camera của Adobe (`rgb_xyz_matrix`; ILCE-7M3: `[0.7374 −0.2389 −0.0551; −0.5435 1.3162 0.2519; −0.1006 0.1795 0.6552]`);
- mức đen/trắng (A7 III: 512 / 15360);
- WB lúc chụp và vùng crop của máy (6000×4000).

Sau đó dựng ma trận đúng như `FUN_009944b0` (Bradford về D65, ×2^(BaselineExposure+0.943)). Nhờ vậy cube nhận cùng loại đầu vào như với 5D: tuyến tính, Rec.709, D65.

```sh
uv run analysis/tools/latent_develop.py DSC08975.ARW P400
uv run analysis/tools/latent_develop.py DSC08975.ARW P400 --matrix dxo       # ma trận sRGB của DxOMark
uv run analysis/tools/latent_develop.py DSC08975.ARW P400 --exposure 0.7     # +EV so với BaselineExposure
#   --matrix adobe|dxo|a,b,c,d,e,f,g,h,i   (9 số: raw đã cân bằng trắng → sRGB tuyến tính, theo hàng)
```

- **Ma trận DxOMark** (`DXO_MATRICES`): raw đã nhân WB → sRGB tuyến tính, đo dưới D50. **Mỗi hàng là một kênh sRGB đầu ra**, tổng mỗi hàng bằng 1. Một số bảng chép lại đặt nhãn ngược (hàng ghi là "R raw").
- So với ma trận Adobe dựng cho cùng WB D50 (2.32/1/1.59), hai bên gần như trùng ở hàng G (`−0.14 1.48 −0.34` / `−0.17 1.58 −0.41`) và hàng B. Khác nhau ở hàng R: Adobe `1.65 −0.51 −0.14`, DxO `2.10 −0.96 −0.14`, nên bản DxO bão hòa đỏ/cam hơn. DxO không thích nghi theo nguồn sáng, nên nó chỉ đúng nhất khi ánh sáng gần D50.
- `BASELINE_EXPOSURE` mặc định là 0 cho máy ngoài 5D. Chưa đo bằng thẻ xám. Với `DSC08975.ARW` (cảnh đêm), ảnh ra tối hơn JPEG nhúng của Sony khoảng 0,5–0,7 EV; JPEG của Sony có nâng vùng tối nên chưa kết luận được.
- Seed grain mặc định là `A:/DCIM/100MSDCF/<TÊN>`. Hạt được sinh theo pixel đầu ra, nên ở 6000×4000 hạt trông mịn hơn so với 4368×2912 của 5D. Hai hệ số halation được khớp theo ảnh 5D, chưa đối chiếu cho Sony.
- App web (mục 6.2) nhận các đuôi `.CR2 .CR3 .ARW .NEF .NRW .DNG .ORF .RW2 .PEF`. CLI không xét đuôi file mà đọc cùng loại container: raw dạng TIFF hoặc CR3. Cả hai đều cần cảm biến Bayer RGGB và LibRaw có ColorMatrix cho máy đó. Không nhận RAF (X-Trans). Make/Model/Orientation lấy từ IFD0 của file TIFF; CR3 không phải TIFF nên lấy từ hộp `CMT1` (một TIFF riêng). Container khác thì CLI báo `unsupported raw container`, còn engine web dùng make/model/flip của LibRaw.
- Màu mới chỉ được kiểm chứng trên 5D, 1D X và A7 III. Các máy khác chạy cùng nhánh LibRaw nhưng `BASELINE_EXPOSURE` mặc định 0, seed grain dùng `A:/DCIM/100CAMERA/<TÊN>` (Canon khác 1D X dùng `100EOS5D`), và chưa so với ảnh thật.

**Canon EOS-1D X** (`IMGL6133.CR2`): CR2 của máy khác 5D không đi theo bảng firmware mà theo nhánh LibRaw ở trên (ColorData 1316 mục, khác 796 của 5D).
- Adobe ColorMatrix: `[0.6847 −0.0614 −0.1014; −0.4669 1.2737 0.2139; −0.1197 0.2488 0.6846]`; đen/trắng 2048 / 15094; crop 5184×3456.
- DxOMark (`--matrix dxo`): `2.22 −1.36 0.15 / −0.26 1.65 −0.39 / 0.05 −0.75 1.70`, WB D50 1.97/1/1.5.
- `BASELINE_EXPOSURE` +0.25 mượn giá trị của 5D, chưa đo. Seed grain mặc định `A:/DCIM/100EOS1D/<TÊN>`.
- Crop của LibRaw tính từ toàn cảm biến; tool trừ `left_margin`/`top_margin` để ra toạ độ trong vùng visible (với A7 III hai giá trị này bằng 0 nên không đổi gì).

```sh
# Ma trận camera → đầu vào cube cho một giá trị WB của CR2 (WB_RGGBLevelsAsShot R G B)
python3 analysis/tools/latent_color.py --autoexec AUTOEXEC.BIN --wb 1966 1024 1491
```

Dùng `.cube` trong phần mềm khác (Resolve, ffmpeg, Nuke…):
1. Đưa ảnh về **RGB tuyến tính, primaries Rec.709/sRGB, D65, scene-referred**. Ví dụ: trong Resolve, Color Space Transform sang Rec.709 / Linear; với ảnh sRGB, bỏ gamma sRGB đi.
2. Chỉnh phơi sáng sao cho **mức xám đo sáng ≈ 0.18**. Nếu đi từ raw (normalized 0..1 theo mức bão hòa), nhân thêm `2^(BaselineExposure + 0.943)`.
3. Cân bằng trắng về D65 (trung tính). Sau đó chọn cube có mired gần với **nhiệt độ màu thật của ánh sáng lúc chụp** nếu muốn giống firmware. Chọn cube 5000–5500K nếu muốn "film ánh sáng ngày, không ám màu".
4. Áp `_shaper.cube` (tuyến tính 0..8 → 0..1), rồi tới `.cube`. Output là giá trị hiển thị sRGB 8 bit / 255.

Còn sai khác so với máy ảnh vì `.cube` xuất ra không chứa:
- nén gamut (`FUN_00994c68`, nằm trước shaper; `latent_develop.py` có làm bước này);
- halation, grain;
- vùng giá trị âm, nằm trong shaper mịn.

Hiệu năng: bản Python giải nén một cube trong khoảng 1 giây.

### 6.2 Giao diện xem trước trên trình duyệt (`app/`, `engine/`)

```sh
cd analysis/app && npm install && npm run dev  # hoặc npm run build, rồi đưa dist/ lên server tĩnh
```

- Không cần Python. Trang chạy pipeline Rust + LibRaw trong WebAssembly, phần theo từng pixel chạy trên WebGPU. Không có WebGPU thì trang dùng đường CPU, chậm hơn khi kéo thanh trượt. `analysis/tools/*.py` vẫn là chuẩn đối chiếu; ảnh tráng ra lệch ±1 ở khoảng 0,0005% giá trị. Cách build, số đo và giới hạn: [engine/README.md](engine/README.md).
- Có `public/LATENT_LITE/` thì trang mở thẳng vào lite pack (mục 6.4). Nếu không, người dùng chọn một thư mục `LATENT_LITE` ngay trong trình duyệt; file được đọc tại chỗ, không upload đi đâu. Kit `.LTP` chỉ dùng được với CLI.
- Có thể chỉnh: film (nhóm theo loại), phơi sáng (±5 EV, như `--exposure` của CLI), giấy/phim in, push/pull, grade giấy B&W, ánh sáng (theo WB lúc chụp hoặc Kelvin), grain, nén gamut, halation (D250/T500), seed grain, chất lượng JPEG.
- Xem trước ở ¼ cỡ (1092×728), ½ cỡ hoặc cỡ đầy đủ. Grain được sinh theo từng pixel đầu ra, nên chỉ ở cỡ đầy đủ hạt mới giống của máy.
- Nút **Export** luôn tráng ở cỡ đầy đủ.
- Engine cache những bước không phụ thuộc thanh trượt: raw đã demosaic, cube đã dựng, khối grain, nhiễu, bản đồ halation. Nếu một lượt preview mới đến khi lượt cũ còn chờ, lượt cũ bị bỏ.
- Phơi sáng nhân vào ma trận camera, tức là trên raw tuyến tính sau khi cắt ở mức bão hòa cảm biến: tăng EV kéo được vùng tối lên (vượt 1.0 thì đi qua shaper và nén gamut của film), giảm EV làm tối ảnh nhưng vùng cảm biến đã cháy vẫn cháy. Bản đồ halation cũng tính theo phơi sáng mới.
- Phím tắt: `[` `]` đổi film, `-` `=` giảm/tăng phơi sáng ⅓ stop, `0` về 0 EV, `Z` chuyển giữa vừa khung và 100%, giữ `C` để so với lần tráng trước.
- Trỏ chuột (hoặc focus bàn phím) vào mã film để xem mô tả (mục 6.3): tên đầy đủ, tag, đường cong tone, thanh điểm. Mô tả đọc từ `traits.json` đi kèm lite pack.
- **Trước khi host công khai**, đọc phần "Khi đưa lên mạng" trong [engine/README.md](engine/README.md): `dist/` chứa dữ liệu film đã giải mã của Latent Studios.

### 6.3 Mô tả tính chất film từ cube (`tools/latent_traits.py`)

```sh
uv run analysis/tools/latent_traits.py [P400 V050 ...] [--json out.json]
```

- Tên đầy đủ lấy từ bảng catalogue của firmware ở `0x9B2254`: 81 bản ghi × 12 byte `{char *tên, char mã[4], u16 ISO, u8 hãng, u8 loại}`. Hãng trỏ vào bảng `0x9B2634` (Agfa, Fujifilm, Ilford, Kodak, Konica), loại trỏ vào bảng `0x9B2620` (colour negative, black and white · C-41, colour reversal, motion picture, black and white). ✔
- Mỗi film được đo theo cùng một điều kiện: giấy #0, push 0, grade index 5, ánh sáng 5500 K (3200 K với film tungsten có tên kết thúc bằng "T"). Dữ liệu đi qua đúng pipeline của `develop()`, chỉ bỏ grain và halation.
- **Tone curve:** thang xám từ −10 đến +7 stop quanh 0.18. Đo L (OKLab) ở điểm đen, xám, trắng; tương phản trung tông = độ dốc L trong khoảng ±1 stop; toe/shoulder = điểm curve đạt 10%/90% khoảng đen→trắng. Xám 0.18 ra khoảng 117/255 ở mọi film âm bản, nên 0.18 đúng là xám trung tính của cube.
- **Ám màu:** a/b (OKLab) của thang xám ở shadow (−4..−2 stop), trung tông, highlight (+1.5..+3 stop).
- **Màu:** 18 ô màu của ColorChecker (giá trị sRGB của BabelColor). Độ bão hòa = chroma/L, so với khi render sRGB thường. Đo riêng cho từng nhóm (da, đỏ, cam, vàng, lục, lam), kèm độ lệch hue.
- Chữ mô tả và thanh điểm (−2..2) là **tương đối so với các film trong `LATENT/`**: 1 đơn vị bằng độ phân tán của bộ film (1.4826 × MAD). Lệch hue và màu nổi bật so với trung vị của từng nhóm, vì cách render sRGB tham chiếu khiến film nào cũng lệch giống nhau. Riêng ám màu (balance) là giá trị tuyệt đối.
- Một số kết quả đáng chú ý: âm bản (Portra, Ektar, Superia) có tương phản trung tông cao hơn hẳn slide và film điện ảnh nhưng highlight chạm trắng sớm (≈ +1.2..+1.6 stop). Vision3 có latitude highlight dài nhất (≈ +2.5 stop) và màu nhạt nhất. Velvia bão hòa nhất. Slide trên "Light box" có điểm trắng chỉ khoảng 200/255. Film B&W có đen bị nâng (giấy Multigrade), tương phản thấp.

### 6.4 Lite pack: bản nhẹ của `.LTP` và `GRAIN.LTG` (`tools/latent_lite.py`, `tools/latent_lite_build.py`)

```sh
uv run analysis/tools/latent_lite_build.py [P400 V050 ...] [--out analysis/app/public/LATENT_LITE] [--jobs 2]
uv run analysis/tools/latent_lite_build.py --keep-cubes      # giữ paper<i>.bin đã có, làm lại phần còn lại
```

- `.LTP` gồm 99,3% là cube LTZ1 đã nén sẵn (khoảng 205 KB mỗi cube, không có khoảng trống), nhưng có tới 19.412 cube. Xuất text sẽ nặng hơn: mỗi `.cube` khoảng 7,2 MB, cả bộ khoảng 144 GB.
- Phần thừa nằm giữa các cube: trong một paper, cube gần như là hàm hạng thấp của push và nhiệt độ màu. Mỗi paper chỉ giữ các cube cơ sở, cộng trọng số trong `film.json`; định dạng ghi ở đầu `latent_lite.py`. Paper thì giữ riêng hết, vì mỗi paper là một màu khác hẳn.
- Kết quả: 3,57 GB (`.LTP` 3,47 GB, `GRAIN.LTG` 98 MB) còn 533 MB; `paper<i>.bin` từ 0,4 đến khoảng 8 MB; `curves.bin` (LTVC) giảm khoảng 94%, không mất dữ liệu.
- Cube của lite pack **có mất mát**. So với `.LTP` qua `develop()`, trước JPEG, không grain, 248 look trên 20 film, ảnh `IMG_0002.jpg` đã bỏ sRGB: lệch trung bình 0,12–0,30 mức trên thang 8-bit, ít nhất 99,86% giá trị lệch ≤ 2 mức, lệch lớn nhất 10 mức. Quét đều khối màu cho lệch lớn nhất 13 mức, ở màu rất bão hòa. V050 và F100 lệch nhiều nhất (khoảng 70 paper trong bộ dừng ở hạng 10×10, trần `MAX_BASIS`). Trên `IMG_0002.CR2` thật, qua cả pipeline rồi JPEG chất lượng 100, 8 look lệch trung bình 0,42–0,61 mức, tối đa 8 mức.
- Grain **không mất mát**: 1.507 khối của mọi `GRAIN.LTG` khớp từng byte. 98 MB còn 40 MB, nhờ lấy sai phân lattice theo 3 trục và theo push, rồi tách byte thấp và byte cao trước zlib. Mỗi `grain<i>.bin` chứa mọi push của paper i, nhiều nhất 0,42 MB.
- `firmware.bin` thay `AUTOEXEC.BIN`: cùng độ dài với payload, chỉ giữ các bảng mà `firmware_ranges` liệt kê (3.337 byte khác 0), phần còn lại là số 0, nên không có code cũng không có khóa. Builder dừng nếu một bảng chạm vùng khóa. Danh mục, ma trận màu, noise grain và thông số halation đều khớp với `AUTOEXEC.BIN`.
- `traits.json` là mô tả film đo sẵn trên chính lite pack; `index.json` liệt kê từng file với cỡ và CRC32 để trang tải qua HTTP (`engine/README.md`).
- Giải mã một paper mất khoảng 1 giây, sau đó khoảng 0,06 giây mỗi cube (bản native), so với 7–9 giây mỗi cube LTZ1 khi giải mã bằng Python. Chỉ một paper được giữ ở dạng đã giải mã, trên mọi pack, vì mỗi paper tốn tới 330 MB.
- Builder cần khoảng 1,5 GB commit cho mỗi film chạy song song. `LATENT_LITE/` là dữ liệu đã giải mã và nằm trong `.gitignore`.

## 7. Còn chưa rõ

- Ý nghĩa của 8000/31250 trong header (firmware chỉ kiểm tra bằng; cũng được ghi vào header `LTCUBE2` mà nó tự dựng trong RAM).
- Mức xám chính xác của ảnh thật: cần một CR2 chụp thẻ xám 18% với đo sáng chuẩn để đo giá trị sau ma trận (dự đoán ≈ 0.18). Bảng 2D WB → sắc độ (`FUN_0099a064`) và bảng quỹ tích ở `0x9A97B0` chưa được trích ra.
- Đơn vị của trục push (−6..+6).
- Halation: vì sao hệ số ×7 và highlight chỉ vào G (đọc từ firmware) không khớp ảnh máy; tool đang dùng hệ số khớp theo một ảnh (mục 5.3). Grain chưa đối chiếu được với JPEG do máy xuất. `.STP` và payload các bản ghi `LTRC`.
- Tên đúng của 40 hàm ROM (cần dump firmware 5D 1.1.1).
