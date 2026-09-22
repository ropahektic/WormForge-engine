"""Local sprite previews from an extracted PNG dump (WA sprite extractors).

Gfx.dir is only opened for .spr headers (frame count). The browser never
hits a live decode path on scroll.
"""

from __future__ import annotations

import os
import struct
import zlib
from pathlib import Path

DIR_MAGIC = 0x1A524944  # DIR\x1A little-endian
DIR_BUCKETS = 1024


def find_gfx_dir() -> Path | None:
    env = os.environ.get("WA_ROOT", "").strip()
    candidates = []
    if env:
        candidates.append(Path(env) / "data" / "Gfx" / "Gfx.dir")
    candidates.extend(
        [
            Path(r"C:\GOG Games\Worms Armageddon\data\Gfx\Gfx.dir"),
            Path(r"C:\Program Files (x86)\Steam\steamapps\common\Worms Armageddon\data\Gfx\Gfx.dir"),
        ]
    )
    for path in candidates:
        if path.is_file():
            return path
    return None


def find_extract_dir() -> Path | None:
    env = os.environ.get("WA_EXTRACT", "").strip()
    candidates = []
    if env:
        candidates.append(Path(env))
    candidates.append(Path(r"C:\Users\Adw11\Desktop\player"))
    for path in candidates:
        if path.is_dir() and any(path.glob("*.png")):
            return path
    return None


def png_size(path: Path) -> tuple[int, int] | None:
    try:
        with path.open("rb") as handle:
            sig = handle.read(24)
    except OSError:
        return None
    if len(sig) < 24 or sig[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    width, height = struct.unpack(">II", sig[16:24])
    if width == 0 or height == 0 or width > 4096 or height > 16384:
        return None
    return width, height


def slice_atlas(png_w: int, png_h: int, spr_frames: int | None) -> tuple[int, int, int]:
    """1-column extractor atlas: (frames, frame_w, frame_h)."""
    if spr_frames and spr_frames > 1 and png_h % spr_frames == 0:
        return spr_frames, png_w, png_h // spr_frames
    if png_w > 0 and png_h > png_w and png_h % png_w == 0:
        count = png_h // png_w
        if 2 <= count <= 256:
            return count, png_w, png_w
    return 1, png_w, png_h


def extract_sprites(extract: Path, gfx: GfxPreview | None, id_by_name: dict[str, int]) -> list[dict]:
    rows = []
    for path in extract.glob("*.png"):
        stem = path.stem.lower()
        # Display-id aliases / stubs — not real pickable sprites.
        if not stem or "#" in stem or stem.endswith("_2"):
            continue
        size = png_size(path)
        if size is None:
            continue
        png_w, png_h = size
        spr_frames = None
        fps = 10
        if gfx is not None:
            try:
                meta = gfx.header(stem)
                spr_frames = meta["frames"]
                fps = meta["fps"] or 10
            except Exception:
                pass
        frames, frame_w, frame_h = slice_atlas(png_w, png_h, spr_frames)
        ident = id_by_name.get(stem)
        if ident is None:
            ident = id_by_name.get(stem.replace("-", "_"))
        rows.append(
            {
                "name": stem,
                "id": ident,
                "frames": frames,
                "fps": fps,
                "fw": frame_w,
                "fh": frame_h,
            }
        )
    rows.sort(key=lambda row: (row["id"] is None, row["id"] or 0, row["name"]))
    return rows


def extract_icons(extract: Path) -> list[dict]:
    folder = extract / "hi"
    if not folder.is_dir():
        return []
    rows = []
    for path in sorted(folder.glob("*.png")):
        size = png_size(path) or (0, 0)
        rows.append({"name": path.stem.lower(), "fw": size[0], "fh": size[1]})
    return rows


def dir_decode(raw: bytes) -> dict[str, tuple[int, int]]:
    if len(raw) < 12:
        raise ValueError("dir too short")
    magic, total, data_size = struct.unpack_from("<III", raw, 0)
    if magic != DIR_MAGIC:
        raise ValueError("not a DIR archive")
    file_end = min(total, len(raw))
    hash_start = data_size + 4
    if hash_start + DIR_BUCKETS * 4 > file_end:
        raise ValueError("dir hash truncated")
    hash_len = file_end - hash_start

    def resolve(encoded: int) -> int | None:
        if encoded == 0:
            return None
        off = encoded - 4
        if off + 12 > hash_len:
            raise ValueError("bad dir offset")
        return hash_start + off

    entries: dict[str, tuple[int, int]] = {}
    for bucket in range(DIR_BUCKETS):
        (encoded,) = struct.unpack_from("<I", raw, hash_start + bucket * 4)
        node = resolve(encoded)
        while node is not None:
            next_raw, value, size = struct.unpack_from("<III", raw, node)
            name_start = node + 12
            zero = raw.find(b"\0", name_start, file_end)
            if zero < 0:
                raise ValueError("unterminated dir name")
            name = raw[name_start:zero].decode("ascii", "replace").lower()
            entries[name] = (value, size)
            node = resolve(next_raw)
    return entries


def align4(n: int) -> int:
    return (n + 3) & ~3


def parse_spr(data: bytes) -> dict:
    if len(data) < 12:
        raise ValueError("spr too short")
    header_flags, palette_count = struct.unpack_from("<HH", data, 8)
    pal_off = 12
    pal_end = pal_off + palette_count * 3
    if len(data) < pal_end:
        raise ValueError("spr palette truncated")
    palette = [data[pal_off + i * 3 : pal_off + i * 3 + 3] for i in range(palette_count)]
    cursor = pal_end
    subframes = []
    if header_flags & 0x4000:
        if len(data) < cursor + 2:
            raise ValueError("spr subframe count truncated")
        (sub_count,) = struct.unpack_from("<H", data, cursor)
        cursor = align4(cursor + 2)
        for _ in range(sub_count):
            compressed_offset, _ptr, decoded_size = struct.unpack_from("<III", data, cursor)
            subframes.append((compressed_offset, decoded_size))
            cursor += 12
    if len(data) < cursor + 12:
        raise ValueError("spr frame header truncated")
    _unk, fps, flags, width, height, frame_count_raw = struct.unpack_from("<HHHHHH", data, cursor)
    cursor += 12
    if frame_count_raw & 0x8000:
        frame_count = 1
    else:
        frame_count = frame_count_raw
    frame_meta = align4(cursor)
    bitmap_off = frame_meta + frame_count * 12
    frames = []
    for i in range(frame_count):
        off = frame_meta + i * 12
        bitmap_offset, start_x, start_y, end_x, end_y = struct.unpack_from("<IHHHH", data, off)
        frames.append((bitmap_offset, start_x, start_y, end_x, end_y))
    return {
        "header_flags": header_flags,
        "palette": palette,
        "fps": fps,
        "flags": flags,
        "width": width,
        "height": height,
        "frames": frames,
        "subframes": subframes,
        "bitmap": data[bitmap_off:],
    }


def lzss_decode(src: bytes, decoded_size: int) -> bytes:
    lut = bytes(range(256))
    dst = bytearray(max(decoded_size + 32, 32))
    src_pos = 0
    dst_pos = 0
    src_len = len(src)
    while src_pos < src_len:
        b = src[src_pos]
        if b & 0x80 == 0:
            if dst_pos < len(dst):
                dst[dst_pos] = lut[b]
            src_pos += 1
            dst_pos += 1
            continue
        if src_pos + 1 >= src_len:
            break
        b1 = src[src_pos + 1]
        distance = ((b << 8) | b1) & 0x7FF
        nibble = (b >> 3) & 0xF
        if nibble != 0:
            copy_offset = distance + 1
            total = nibble + 2
            for _ in range(total):
                v = dst[dst_pos - copy_offset] if 0 <= dst_pos - copy_offset < len(dst) else 0
                if dst_pos < len(dst):
                    dst[dst_pos] = v
                dst_pos += 1
            src_pos += 2
        else:
            if distance == 0:
                break
            if src_pos + 2 >= src_len:
                break
            length = src[src_pos + 2] + 18
            copy_offset = distance
            for _ in range(length):
                v = dst[dst_pos - copy_offset] if 0 <= dst_pos - copy_offset < len(dst) else 0
                if dst_pos < len(dst):
                    dst[dst_pos] = v
                dst_pos += 1
            src_pos += 3
    return bytes(dst[: max(decoded_size, dst_pos)])


def png_rgba(width: int, height: int, rgba: bytes) -> bytes:
    def chunk(tag: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(tag + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + tag + payload + struct.pack(">I", crc)

    raw = b"".join(b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")


def indices_to_rgba(width: int, height: int, indices: bytes, palette: list[bytes], start_x=0, start_y=0, end_x=None, end_y=None) -> bytes:
    if end_x is None:
        end_x = width
    if end_y is None:
        end_y = height
    fw = max(0, end_x - start_x)
    fh = max(0, end_y - start_y)
    out = bytearray(width * height * 4)
    pal = [(0, 0, 0, 0)] + [(p[0], p[1], p[2], 255) for p in palette]
    for y in range(fh):
        for x in range(fw):
            src = y * fw + x
            if src >= len(indices):
                continue
            idx = indices[src]
            r, g, b, a = pal[idx] if idx < len(pal) else (0, 0, 0, 0)
            dx = start_x + x
            dy = start_y + y
            if dx >= width or dy >= height:
                continue
            o = (dy * width + dx) * 4
            out[o : o + 4] = bytes((r, g, b, a))
    return bytes(out)


class GfxPreview:
    def __init__(self, path: Path):
        self.path = path
        self.raw = path.read_bytes()
        self.entries = dir_decode(self.raw)
        self._png: dict[tuple[str, int], bytes] = {}
        self._meta: dict[str, dict] = {}
        self._parsed: dict[str, dict] = {}

    def names(self) -> list[str]:
        return sorted(self.entries)

    def spr_stems(self) -> list[str]:
        return [n[:-4] for n in self.entries if n.endswith(".spr")]

    def catalog(self, id_by_name: dict[str, int]) -> list[dict]:
        rows = []
        for stem in self.spr_stems():
            ident = id_by_name.get(stem)
            if ident is None:
                ident = id_by_name.get(stem.replace("-", "_"))
            try:
                meta = self.meta(stem)
                frames = meta["frames"]
                fps = meta["fps"] or 10
            except Exception:
                frames, fps = 1, 10
            rows.append(
                {
                    "name": stem,
                    "id": ident,
                    "frames": frames,
                    "fps": fps,
                }
            )
        rows.sort(key=lambda r: (r["id"] is None, r["id"] or 0, r["name"]))
        return rows

    def spr_bytes(self, stem: str) -> bytes:
        key = stem.lower()
        if not key.endswith(".spr"):
            key = f"{key}.spr"
        if key not in self.entries and "-" in key:
            alt = key.replace("-", "_")
            if alt in self.entries:
                key = alt
        if key not in self.entries and "_" in key:
            alt = key.replace("_", "-")
            if alt in self.entries:
                key = alt
        if key not in self.entries:
            raise KeyError(stem)
        off, size = self.entries[key]
        return self.raw[off : off + size]

    def header(self, stem: str) -> dict:
        key = stem.lower()
        if key not in self._meta:
            parsed = parse_spr(self.spr_bytes(key))
            self._meta[key] = {
                "width": parsed["width"],
                "height": parsed["height"],
                "frames": len(parsed["frames"]),
                "fps": parsed["fps"] or 10,
                "compressed": bool(parsed["header_flags"] & 0x4000),
            }
        return self._meta[key]

    def meta(self, stem: str) -> dict:
        return self.header(stem)

    def _parsed_spr(self, stem: str) -> dict:
        key = stem.lower()
        if key not in self._parsed:
            self._parsed[key] = parse_spr(self.spr_bytes(key))
        return self._parsed[key]

    def frame_png(self, stem: str, frame: int = 0) -> bytes:
        key = (stem.lower(), int(frame))
        if key in self._png:
            return self._png[key]
        parsed = self._parsed_spr(stem)
        frames = parsed["frames"]
        if not frames:
            raise ValueError("no frames")
        idx = int(frame) % len(frames)
        bitmap_offset, start_x, start_y, end_x, end_y = frames[idx]
        fw = max(1, end_x - start_x)
        fh = max(1, end_y - start_y)
        if parsed["header_flags"] & 0x4000:
            sub_i = (bitmap_offset >> 24) & 0xFF
            if sub_i >= 128:
                sub_i -= 256
            pix_off = bitmap_offset & 0xFFFFFF
            compressed_offset, decoded_size = parsed["subframes"][sub_i]
            decoded = lzss_decode(parsed["bitmap"][compressed_offset:], decoded_size)
            indices = decoded[pix_off : pix_off + fw * fh]
        else:
            indices = parsed["bitmap"][bitmap_offset : bitmap_offset + fw * fh]
        rgba = indices_to_rgba(
            parsed["width"],
            parsed["height"],
            indices,
            parsed["palette"],
            start_x,
            start_y,
            end_x,
            end_y,
        )
        png = png_rgba(parsed["width"], parsed["height"], rgba)
        self._png[key] = png
        return png


_PREVIEW: GfxPreview | None = None
_PREVIEW_ERROR: str | None = None
_EXTRACT: Path | None = None
_EXTRACT_TRIED = False


def get_preview() -> GfxPreview | None:
    global _PREVIEW, _PREVIEW_ERROR
    if _PREVIEW is not None or _PREVIEW_ERROR:
        return _PREVIEW
    path = find_gfx_dir()
    if path is None:
        _PREVIEW_ERROR = "Gfx.dir not found (set WA_ROOT or install WA)"
        return None
    try:
        _PREVIEW = GfxPreview(path)
    except Exception as exc:  # noqa: BLE001
        _PREVIEW_ERROR = str(exc)
        return None
    return _PREVIEW


def get_extract() -> Path | None:
    global _EXTRACT, _EXTRACT_TRIED
    if _EXTRACT_TRIED:
        return _EXTRACT
    _EXTRACT_TRIED = True
    _EXTRACT = find_extract_dir()
    return _EXTRACT
