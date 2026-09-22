"""Local weapon-creator: pick a template, download a Mods pack zip."""

from __future__ import annotations

import json
import posixpath
import re
import sys
import zipfile
from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler
from io import BytesIO
from pathlib import Path
from urllib.parse import unquote, urlparse

HERE = Path(__file__).resolve().parent
PX_GIFS = Path(r"C:\Users\Adw11\Downloads\px gifs\pxl_gifs")
PX_GIF_RE = re.compile(r"^[A-Za-z0-9._-]+\.gif$", re.IGNORECASE)
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))
from gfx_preview import extract_icons, extract_sprites, get_extract, get_preview
ROOT = HERE.parents[1]
TEMPLATES_PATH = ROOT / "crates" / "weapons" / "fire_templates.json"
SPRITES_PATH = ROOT / "crates" / "gfx" / "stock_sprites.json"
HOST = "127.0.0.1"
PORT = 8765

SLOTS = [
    "bazooka",
    "homing_missile",
    "mortar",
    "homing_pigeon",
    "sheep_launcher",
    "grenade",
    "cluster_bomb",
    "banana_bomb",
    "battle_axe",
    "earthquake",
    "shotgun",
    "handgun",
    "uzi",
    "minigun",
    "longbow",
    "fire_punch",
    "dragon_ball",
    "kamikaze",
    "suicide_bomber",
    "prod",
    "dynamite",
    "mine",
    "sheep",
    "super_sheep",
    "aqua_sheep",
    "mole_bomb",
    "air_strike",
    "napalm_strike",
    "mail_strike",
    "mine_strike",
    "mole_squadron",
    "blow_torch",
    "pneumatic_drill",
    "girder",
    "baseball_bat",
    "girder_pack",
    "ninja_rope",
    "bungee",
    "parachute",
    "teleport",
    "scales_of_justice",
    "super_banana",
    "holy_grenade",
    "flame_thrower",
    "salvation_army",
    "mb_bomb",
    "petrol_bomb",
    "skunk",
    "ming_vase",
    "sheep_strike",
    "carpet_bomb",
    "mad_cow",
    "old_woman",
    "donkey",
    "nuclear_test",
    "armageddon",
    "skip_go",
    "surrender",
    "select_worm",
    "freeze",
    "magic_bullet",
    "jet_pack",
    "low_gravity",
    "fast_walk",
    "laser_sight",
    "invisibility",
    "damage_x2",
    "crate_spy",
    "double_turn_time",
    "crate_shower",
]

ID_RE = re.compile(r"^[a-z][a-z0-9._-]{0,62}$")


def gif_size(path: Path) -> tuple[int, int]:
    try:
        header = path.read_bytes()[:10]
    except OSError:
        return 32, 32
    if len(header) >= 10 and header[:6] in (b"GIF87a", b"GIF89a"):
        w = int.from_bytes(header[6:8], "little")
        h = int.from_bytes(header[8:10], "little")
        if w and h:
            return w, h
    return 32, 32


def list_px_sprites() -> list[dict]:
    if not PX_GIFS.is_dir():
        return []
    out = []
    for path in PX_GIFS.iterdir():
        if not path.is_file():
            continue
        name = path.name
        if not PX_GIF_RE.fullmatch(name):
            continue
        fw, fh = gif_size(path)
        out.append(
            {
                "name": name[:-4],
                "file": name,
                "path": f"sprites/{name}",
                "fw": fw,
                "fh": fh,
                "frames": 1,
            }
        )
    out.sort(key=lambda row: row["name"].lower())
    return out


def px_file_from_sprite(raw: str) -> str | None:
    name = raw.replace("\\", "/").strip()
    if name.startswith("sprites/"):
        name = name[len("sprites/") :]
    if "/" in name or "\\" in name:
        name = name.rsplit("/", 1)[-1]
    if not PX_GIF_RE.fullmatch(name):
        return None
    if not (PX_GIFS / name).is_file():
        return None
    return name


def collect_px_files(lua: str, projectile: dict) -> list[str]:
    found: set[str] = set()
    for match in re.finditer(r"sprites/([A-Za-z0-9._-]+\.gif)", lua, re.IGNORECASE):
        found.add(match.group(1))

    def walk(value):
        if isinstance(value, dict):
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)
        elif isinstance(value, str):
            name = px_file_from_sprite(value)
            if name:
                found.add(name)

    walk(projectile)
    return sorted(name for name in found if (PX_GIFS / name).is_file())


def load_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def lua_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def slug_folder(mod_id: str) -> str:
    part = mod_id.rsplit(".", 1)[-1]
    return re.sub(r"[^a-z0-9_-]", "_", part) or "weapon"


def optional_i32(raw):
    if raw is None or raw == "":
        return None
    return int(raw)


SLOT_ICONS = {
    "bazooka": "bazooka",
    "homing_missile": "hmissile",
    "mortar": "mortar",
    "homing_pigeon": "pigeon",
    "sheep_launcher": "launch",
    "grenade": "grenade",
    "cluster_bomb": "cluster",
    "banana_bomb": "banana",
    "battle_axe": "axe",
    "earthquake": "quake",
    "shotgun": "shotgun",
    "handgun": "handgun",
    "uzi": "uzi",
    "minigun": "minigun",
    "longbow": "longbow",
    "fire_punch": "firepnch",
    "dragon_ball": "dragball",
    "kamikaze": "kamikaze",
    "suicide_bomber": "suicide",
    "prod": "prod",
    "dynamite": "dynamite",
    "mine": "mine",
    "sheep": "sheep",
    "super_sheep": "sprsheep",
    "aqua_sheep": "aqua",
    "mole_bomb": "mole",
    "air_strike": "airstrke",
    "napalm_strike": "firestrk",
    "mail_strike": "postal",
    "mine_strike": "minestrk",
    "mole_squadron": "molestrk",
    "blow_torch": "blwtorch",
    "pneumatic_drill": "drill",
    "girder": "girder",
    "baseball_bat": "baseball",
    "girder_pack": "girders",
    "ninja_rope": "rope",
    "bungee": "bungee",
    "parachute": "parachut",
    "teleport": "teleport",
    "scales_of_justice": "scales",
    "super_banana": "sbanana",
    "holy_grenade": "hgrenade",
    "flame_thrower": "thrower",
    "salvation_army": "tamborin",
    "mb_bomb": "mbbomb",
    "petrol_bomb": "petrolbm",
    "skunk": "skunk",
    "ming_vase": "mingvase",
    "sheep_strike": "shpstrke",
    "carpet_bomb": "carpet",
    "mad_cow": "cow",
    "old_woman": "oldwoman",
    "donkey": "donkey",
    "nuclear_test": "nuke",
    "armageddon": "armagedn",
    "skip_go": "skipgo",
    "surrender": "surender",
    "select_worm": "select",
    "freeze": "freeze",
    "magic_bullet": "bullet",
    "jet_pack": "jetpack",
    "low_gravity": "gravity",
    "fast_walk": "speed",
    "laser_sight": "laser",
    "invisibility": "invisibl",
    "damage_x2": "damagex2",
    "crate_spy": "cratespy",
    "double_turn_time": "clone",
    "crate_shower": "canister",
}

ALIASES = {
    "bazooka": "missile",
    "holy": "hgrenade",
    "holy_grenade": "hgrenade",
    "mine": "mineon",
    "cursor": "cursorr",
    "homing": "hmissil1",
    "homing_missile": "hmissil1",
    "pigeon": "pigeonr0",
}


def sprite_exists(name: str) -> bool:
    gfx = get_preview()
    if gfx is not None:
        try:
            gfx.spr_bytes(name)
            return True
        except KeyError:
            return False
    names = {s["name"] for s in load_json(SPRITES_PATH)}
    return name in names or name.replace("-", "_") in names


def is_pack_sprite(name: str) -> bool:
    n = name.replace("\\", "/").lower()
    return "/" in n or n.endswith(".png") or n.endswith(".gif")


def emit_sprite_name(raw, default: str | None = None) -> str:
    sprite = str(raw or "").strip()
    if not sprite:
        sprite = str(default or "").strip()
    if not sprite:
        return ""
    packed = px_file_from_sprite(sprite)
    if packed:
        return f"sprites/{packed}"
    if is_pack_sprite(sprite):
        raise ValueError(f"unknown sprite '{sprite}'")
    return resolve_sprite(sprite.lower())


def resolve_sprite(name: str) -> str:
    resolved = ALIASES.get(name, name)
    if not sprite_exists(resolved):
        raise ValueError(f"unknown sprite '{name}'")
    return resolved


def infer_copy_from(fire: str, body: dict, teleport: bool) -> str:
    if fire == "hitscan":
        return "uzi"
    if fire == "drop":
        return "dynamite" if body.get("fuse") else "mine"
    if fire == "cursor":
        return "teleport" if teleport else "air_strike"
    if body.get("cluster"):
        return "cluster_bomb"
    homing = body.get("homing") or "off"
    if homing == "avoid":
        return "homing_pigeon"
    if homing == "dodge":
        return "magic_bullet"
    if homing != "off":
        return "homing_missile"
    if body.get("fuse"):
        return "grenade"
    return "bazooka"


def emit_params(body: dict, indent: str) -> list[str]:
    rows = []
    for key in ("damage", "gravity_pct", "wind_pct", "bounce_pct"):
        value = optional_i32(body.get(key))
        if value is not None:
            rows.append(f"{indent}  {key} = {value},")
    if not rows:
        return []
    return [f"{indent}params = {{", *rows, f"{indent}}},"]


def emit_sprite_and_params(body: dict, indent: str) -> list[str]:
    lines = []
    sprite = emit_sprite_name(body.get("sprite"))
    if sprite:
        lines.append(f"{indent}sprite = {lua_string(sprite)},")
    lines.extend(emit_params(body, indent))
    return lines


def emit_cluster_bit(body: dict, indent: str) -> list[str]:
    lines = emit_sprite_and_params(body, indent)
    homing = body.get("homing") or "off"
    if homing != "off":
        lines.append(f"{indent}homing = {lua_string(homing)},")
    return lines


def emit_root(fire: str, body: dict) -> list[str]:
    if fire == "hitscan":
        value = optional_i32(body.get("damage"))
        if value is None:
            return []
        return ["  params = {", f"    damage = {value},", "  },"]
    lines = []
    if fire in ("power", "drop"):
        lines.extend(emit_sprite_and_params(body, "  "))
    if fire == "power" and body.get("cluster"):
        child = body.get("child") if isinstance(body.get("child"), dict) else {}
        lines.append("  cluster = {")
        lines.extend(emit_cluster_bit(child, "    "))
        lines.append("  },")
    if fire == "drop" and not body.get("fuse"):
        rows = []
        mapping = (("proximity", "proximity"), ("fuse_ms", "fuse"), ("mask", "mask"), ("trigger", "trigger"))
        for src, dst in mapping:
            value = optional_i32(body.get(src))
            if value is not None:
                rows.append(f"    {dst} = {value},")
        if rows:
            lines.append("  mine = {")
            lines.extend(rows)
            lines.append("  },")
        if body.get("place_on"):
            sprite = emit_sprite_name(body.get("place_sprite"), "crshairb")
            lines.extend(
                [
                    "  on_place = {",
                    "    spawn = {",
                    f"      sprite = {lua_string(sprite)},",
                    "      gravity = false,",
                    "      collide = false,",
                    "      persist = true,",
                    "      drown = true,",
                    "    },",
                    "  },",
                ]
            )
        if body.get("prox_on"):
            prox_id = optional_i32(body.get("prox_id"))
            if prox_id is None:
                prox_id = 25
            lines.extend(
                [
                    "  on_proximity = {",
                    "    spawn = {",
                    "      sprite = false,",
                    f"      explode = {{ damage = 0, id = {prox_id}, now = true }},",
                    "      damage_worms = false,",
                    "      damage_terrain = true,",
                    "    },",
                    "  },",
                ]
            )
        if body.get("boom_on"):
            sprite = emit_sprite_name(body.get("boom_sprite"), "banana")
            count = optional_i32(body.get("boom_count"))
            if count is None:
                count = 5
            spread = optional_i32(body.get("boom_spread"))
            if spread is None:
                spread = 45
            power = optional_i32(body.get("boom_power"))
            if power is None:
                power = 0
            dmg = optional_i32(body.get("boom_damage"))
            if dmg is None:
                dmg = 30
            trail = emit_sprite_name(body.get("boom_trail"), "smklt25")
            impact = body.get("boom_impact")
            if impact is None:
                impact = True
            bounce = bool(body.get("boom_bounce"))
            homing = str(body.get("boom_homing") or "off")
            boom_lines = [
                "  on_explode = {",
                "    spawn = {",
                "      kind = \"cluster\",",
                f"      sprite = {lua_string(sprite)},",
                f"      count = {count},",
                f"      spread = {spread},",
                f"      power = {power},",
                f"      trail = {lua_string(trail)},",
                f"      impact = {'true' if impact else 'false'},",
                f"      bounce = {'true' if bounce else 'false'},",
            ]
            if homing and homing != "off":
                boom_lines.append(f"      homing = {lua_string(homing)},")
            else:
                boom_lines.append("      homing = false,")
            boom_lines.extend(
                [
                    "      gravity = true,",
                    "      collide = true,",
                    "      persist = false,",
                ]
            )
            if not impact:
                boom_lines.append("      damage_worms = false,")
            if impact:
                boom_lines.append(f"      explode = {{ damage = {dmg}, id = 100 }},")
            boom_lines.extend(
                [
                    "    },",
                    "  },",
                ]
            )
            lines.extend(boom_lines)
    return lines


def accept_lua(raw) -> str:
    if not isinstance(raw, str):
        raise ValueError("lua must be the weapons.lua text")
    lua = raw.replace("\r\n", "\n")
    if len(lua) > 80_000:
        raise ValueError("lua too large")
    if "\x00" in lua:
        raise ValueError("lua invalid")
    if "wa.weapons.replace" not in lua:
        raise ValueError("lua must be a weapons.replace pack")
    if not lua.endswith("\n"):
        lua += "\n"
    return lua


def build_pack(body: dict) -> tuple[str, bytes]:
    if isinstance(body.get("body"), dict):
        fire = str(body.get("fire") or "power")
        projectile = body["body"]
        copy_from = str(body.get("copy_from") or infer_copy_from(fire, projectile, bool(body.get("teleport"))))
        mod_id = str(body.get("id") or "").strip().lower()
        if not ID_RE.match(mod_id):
            raise ValueError("id must look like user.my_weapon")
        name = str(body.get("name") or "").strip()
        if not name or len(name) > 40:
            raise ValueError("name is required (max 40 chars)")
        slot = str(body.get("slot") or copy_from)
        if slot not in SLOTS:
            raise ValueError("unknown panel slot")
        if copy_from not in SLOTS:
            raise ValueError("unknown copy_from")
        if "lua" in body:
            lua = accept_lua(body.get("lua"))
        else:
            lua_lines = [
                f"-- copy_from {copy_from}. Only fields the overlay actually writes are emitted.",
                "",
                "wa.weapons.replace({",
                f"  weapon = {lua_string(slot)},",
                f"  name = {lua_string(name)},",
                f"  copy_from = {lua_string(copy_from)},",
            ]
            if fire == "cursor" and body.get("teleport"):
                lua_lines.append("  on_select = { cursor = true },")
            icon = str(body.get("panel_icon") or "").strip().lower()
            if icon:
                lua_lines.append(f"  panel_icon = {lua_string(icon)},")
            lua_lines.extend(emit_root(fire, projectile))
            lua_lines.append("})")
            lua_lines.append("")
            lua = "\n".join(lua_lines)
        toml = "\n".join(
            [
                f"id = {lua_string(mod_id)}",
                'version = "0.1.0"',
                'author = "weapon-creator"',
                "api_version = 1",
                'entry = "weapons.lua"',
                "",
                "[weapons]",
                f"register = [{lua_string(mod_id)}]",
                f"replace = [{lua_string(slot)}]",
                "",
                "[actors]",
                "register = []",
                "",
            ]
        )
        folder = slug_folder(mod_id)
        buf = BytesIO()
        with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
            zf.writestr(f"{folder}/mod.toml", toml)
            zf.writestr(f"{folder}/weapons.lua", lua)
            for name in collect_px_files(lua, projectile):
                zf.write(PX_GIFS / name, f"{folder}/sprites/{name}")
        return folder, buf.getvalue()

    raise ValueError("expected a weapon tree (body)")


_CATALOG = None


def catalog_payload() -> dict:
    global _CATALOG
    if _CATALOG is not None:
        return _CATALOG
    gfx = get_preview()
    extract = get_extract()
    id_by_name = {row["name"]: row["id"] for row in load_json(SPRITES_PATH)}
    if extract is not None:
        sprites = extract_sprites(extract, gfx, id_by_name)
        icons = extract_icons(extract)
    elif gfx is not None:
        sprites = gfx.catalog(id_by_name)
        icons = []
    else:
        sprites = load_json(SPRITES_PATH)
        icons = []
    _CATALOG = {
        "templates": load_json(TEMPLATES_PATH),
        "sprites": sprites,
        "px_sprites": list_px_sprites(),
        "icons": icons,
        "slot_icons": SLOT_ICONS,
        "slots": SLOTS,
        "extract": None if extract is None else str(extract),
        "gfx": None if gfx is None else str(gfx.path),
    }
    return _CATALOG


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(HERE), **kwargs)

    def log_message(self, fmt, *args):
        print(f"[weapon-creator] {self.address_string()} {fmt % args}")

    def do_GET(self):
        path = urlparse(self.path).path
        if path in ("/", "/index.html", "/app.js", "/aim_bake.js"):
            return SimpleHTTPRequestHandler.do_GET(self)
        if path == "/api/catalog":
            return self._json(200, catalog_payload())
        if path.startswith("/px/") and path.lower().endswith(".gif"):
            return self._px_gif(path)
        if path.startswith("/extract/") and path.endswith(".png"):
            return self._extract_png(path)
        if path == "/health":
            gfx = get_preview()
            extract = get_extract()
            return self._json(
                200,
                {
                    "ok": True,
                    "extract": None if extract is None else str(extract),
                    "gfx": None if gfx is None else str(gfx.path),
                },
            )
        self.send_error(404)

    def do_POST(self):
        path = urlparse(self.path).path
        if path != "/api/export":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length") or 0)
        if length > 256_000:
            return self._json(400, {"error": "payload too large"})
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw.decode("utf-8"))
            folder, blob = build_pack(body)
        except (json.JSONDecodeError, ValueError, TypeError, KeyError) as exc:
            return self._json(400, {"error": str(exc)})
        self.send_response(200)
        self.send_header("Content-Type", "application/zip")
        self.send_header(
            "Content-Disposition",
            f'attachment; filename="{posixpath.basename(folder)}.zip"',
        )
        self.send_header("Content-Length", str(len(blob)))
        self.end_headers()
        self.wfile.write(blob)

    def _extract_png(self, path: str):
        extract = get_extract()
        if extract is None:
            self.send_error(503, "extracted sprite folder not found")
            return
        rel = unquote(path[len("/extract/") :]).replace("\\", "/").lstrip("/")
        parts = [p for p in rel.split("/") if p and p not in (".", "..")]
        if not parts or not parts[-1].endswith(".png"):
            self.send_error(400, "bad extract path")
            return
        for part in parts:
            stem = part[:-4] if part.endswith(".png") else part
            if not re.fullmatch(r"[a-z][a-z0-9_-]*", stem):
                self.send_error(400, "bad extract name")
                return
        if len(parts) == 1:
            target = extract / parts[0]
        elif len(parts) == 2 and parts[0] in ("hi", "lo"):
            target = extract / parts[0] / parts[1]
        else:
            self.send_error(400, "bad extract path")
            return
        try:
            target = target.resolve()
            target.relative_to(extract.resolve())
        except (OSError, ValueError):
            self.send_error(400, "bad extract path")
            return
        if not target.is_file():
            self.send_error(404, "extracted png missing")
            return
        blob = target.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Cache-Control", "public, max-age=86400")
        self.send_header("Content-Length", str(len(blob)))
        self.end_headers()
        self.wfile.write(blob)

    def _px_gif(self, path: str):
        rel = unquote(path[len("/px/") :]).replace("\\", "/")
        name = posixpath.basename(rel)
        if not PX_GIF_RE.fullmatch(name):
            self.send_error(400, "bad px gif name")
            return
        target = (PX_GIFS / name).resolve()
        try:
            target.relative_to(PX_GIFS.resolve())
        except ValueError:
            self.send_error(400, "bad px gif path")
            return
        if not target.is_file():
            self.send_error(404, "px gif missing")
            return
        blob = target.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "image/gif")
        self.send_header("Cache-Control", "public, max-age=86400")
        self.send_header("Content-Length", str(len(blob)))
        self.end_headers()
        self.wfile.write(blob)

    def _json(self, status: int, payload: dict):
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    extract = get_extract()
    gfx = get_preview()
    cat = catalog_payload()
    httpd = ThreadingHTTPServer((HOST, PORT), Handler)
    httpd.allow_reuse_address = False
    print(f"weapon creator http://{HOST}:{PORT}", flush=True)
    print(f"extract {extract} ({len(cat['sprites'])} atlases, {len(cat['icons'])} panel icons)", flush=True)
    print(f"px sprites {PX_GIFS} ({len(cat['px_sprites'])} gifs)", flush=True)
    print(f"gfx {None if gfx is None else gfx.path}", flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
