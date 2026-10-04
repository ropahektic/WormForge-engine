#!/usr/bin/env python3
"""Split Highlander's single rules.lua into pack modules, mechanically.

    python3 tools/split_highlander.py <rules.lua> <pack dir>

Reads one monolithic rules.lua and writes, into the pack directory, the entry
rules.lua (the header comment and one `require` per module, in file order)
and the modules it requires. The split changes no behaviour, and it can be
rerun on any later version of the monolith, so a change made to the single
file (an upstream review, say) is regenerated rather than merged by hand.

Modules start at anchor lines (MODULES below); each anchor must match exactly
once and every cut must fall outside all blocks. A module is the code up to
the next anchor, unchanged except:

  * A file-level local that a later module uses is exported at the end of
    the module that declares it (`shared.name = name`) and imported at the
    top of each later module that names it (`local name = shared.name`),
    which keeps Lua's lexical meaning: a later module saw the same binding,
    with any shadowing of its own.
  * A file-level local that is assigned again outside its own module, or
    inside a function, cannot be a frozen copy: every reference to it is
    rewritten to `shared.name`, and its declaration with it. The tool refuses
    if such a name is ever declared again, as a local or a parameter.

`shared` is lib/shared.lua's table, one per match (a module runs once and
`require` returns the same value to every caller).
"""

import re
import sys
from pathlib import Path

# (anchor, module path, what it holds). The first anchor ends the header.
MODULES = [
    (r"^-- -{8,} weapons$", "lib/holds", "the weapon holds the engine draws"),
    (r"^-- Filled in by the Double Bazooka", "weapons/catalog", "the claimed weapons and their art"),
    (r"^-- WEP_PATCH begin$", "weapons/wep_data", "names and record patches (tools/embed_weps.py)"),
    (r"^for _, list in ipairs\(\{ PORTED, PX_ART \}\) do$", "weapons/claims", "the slot claims"),
    (r"^-- The two with a script of their own", "weapons/super_drill", "Super Drill"),
    (r"^-- Fire Jump \(PX Fire_Jump_script\)", "weapons/fire_jump", "Fire Jump"),
    (r"^-- PX's missile sprite animations share", "lib/anim", "sprite animation helpers"),
    (r"^-- The Sniper Rifle's sight remains", "weapons/sniper_rifle", "the Sniper Rifle's laser sight"),
    (r"^-- -{8,} PX's missile sprites, at run time$", "lib/missile_sprites", "PX's missile sprites"),
    (r"^-- -{8,} Double Bazooka$", "weapons/double_bazooka", "Double Bazooka"),
    (r"^-- -{8,} Chilli Bomb$", "weapons/chilli_bomb", "Chilli Bomb"),
    (r"^-- -{8,} Nuke Rocket$", "weapons/nuke_rocket", "Nuke Rocket"),
    (r"^-- -{8,} Helicopter$", "weapons/helicopter", "Helicopter"),
    (r"^-- -{8,} Hot Air Balloon$", "weapons/hot_air_balloon", "Hot Air Balloon"),
    (r"^-- -{8,} Hang-Glider$", "weapons/hang_glider", "Hang-Glider"),
    (r"^-- -{8,} Swim Suit$", "weapons/swim_suit", "Swim Suit"),
    (r"^-- -{8,} Wind Changer, Gravity Control$", "weapons/wind_gravity", "Wind Changer, Gravity Control"),
    (r"^-- -{8,} Jump$", "weapons/jump", "Jump"),
    (r"^-- -{8,} Laser Gun$", "weapons/laser_gun", "Laser Gun"),
    (r"^-- -{8,} pools$", "scheme/pools", "the scheme's weapon pools"),
    (r"^-- -{8,} state$", "scheme/state", "the scheme's state"),
    (r"^-- -{8,} rules$", "scheme/rules", "the scheme's rules"),
    (r"^-- -{8,} Super Tele$", "weapons/super_tele", "Super Tele"),
    (r"^-- -{8,} electrical weapons$", "weapons/electrical", "the electrical weapons"),
    (r"^-- -{8,} Javelin$", "weapons/javelin", "Javelin"),
]
# The scheme's own modules: the test pack (tools/build_highlander_test.py)
# runs every other one.
SCHEME = {"scheme/pools", "scheme/state", "scheme/rules"}
SHARED = "shared"

KEYWORDS = {
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function",
    "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return",
    "then", "true", "until", "while",
}
OPS = ("...", "..", "==", "~=", "<=", ">=", "::", "//", "<<", ">>")


def tokens(src):
    """(kind, text, start, end) for names, strings, numbers and operators."""
    i, n = 0, len(src)
    long_open = re.compile(r"\[(=*)\[")
    out = []
    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
        elif src.startswith("--", i):
            m = long_open.match(src, i + 2)
            if m:
                j = src.find("]" + m.group(1) + "]", m.end())
                i = n if j < 0 else j + len(m.group(1)) + 2
            else:
                j = src.find("\n", i)
                i = n if j < 0 else j
        elif c in "\"'":
            j = i + 1
            while j < n and src[j] != c:
                j += 2 if src[j] == "\\" else 1
            out.append(("str", src[i:j + 1], i, j + 1))
            i = j + 1
        elif long_open.match(src, i):
            m = long_open.match(src, i)
            j = src.find("]" + m.group(1) + "]", m.end())
            j = n if j < 0 else j + len(m.group(1)) + 2
            out.append(("str", src[i:j], i, j))
            i = j
        elif c.isalpha() or c == "_":
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            out.append(("name", src[i:j], i, j))
            i = j
        elif c.isdigit() or (c == "." and i + 1 < n and src[i + 1].isdigit()):
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] in "._" or (src[j] in "+-" and src[j - 1] in "eEpP")):
                j += 1
            out.append(("num", src[i:j], i, j))
            i = j
        else:
            op = next((o for o in OPS if src.startswith(o, i)), c)
            out.append(("op", op, i, i + len(op)))
            i += len(op)
    return out


class Scan:
    """Every name occurrence, classified, with its block depth and frames."""

    def __init__(self, src):
        self.toks = tokens(src)
        # (index, kind, name): kind is decl0 (file-level local), decl (nested
        # local, parameter or loop variable), assign (bare assignment), use.
        self.refs = []
        self.depth_at = {}  # token index -> (block depth, inside a function)
        self._walk()

    def _walk(self):
        toks = self.toks
        stack = []  # "block", "fn", "{", "(", "["
        pending_params = False
        k = 0
        while k < len(toks):
            kind, text, s, e = toks[k]
            prev = toks[k - 1][1] if k else ""
            nxt = toks[k + 1][1] if k + 1 < len(toks) else ""
            blocks = sum(1 for f in stack if f in ("block", "fn"))
            in_fn = "fn" in stack
            self.depth_at[k] = (blocks, in_fn)
            if kind == "op":
                if text in "{([":
                    stack.append(text)
                elif text in "})]":
                    assert stack and stack[-1] == {"}": "{", ")": "(", "]": "["}[text], f"unbalanced {text} at {s}"
                    stack.pop()
                k += 1
                continue
            if kind != "name":
                k += 1
                continue
            if text in ("do", "then", "repeat"):
                if text == "do" and stack and stack[-1] == "loophead":
                    stack[-1] = "block"
                elif text == "then":
                    pass  # `if` already opened the block
                else:
                    stack.append("block")
                k += 1
                continue
            if text in ("if",):
                stack.append("block")
                k += 1
                continue
            if text in ("while", "for"):
                stack.append("loophead")
                if text == "for":
                    # loop variables up to `=` or `in`
                    j = k + 1
                    while toks[j][1] not in ("=", "in"):
                        if toks[j][0] == "name":
                            self.depth_at[j] = (blocks + 1, in_fn)
                            self.refs.append((j, "decl", toks[j][1]))
                        j += 1
                    k = j
                    continue
                k += 1
                continue
            if text in ("end", "until"):
                assert stack and stack[-1] in ("block", "fn"), f"unbalanced {text} at {s}"
                stack.pop()
                k += 1
                continue
            if text == "function":
                # name (for a statement), then parameters
                j = k + 1
                if toks[j][0] == "name":
                    self.depth_at[j] = (blocks, in_fn)
                    if toks[j + 1][1] in (".", ":"):
                        self.refs.append((j, "use", toks[j][1]))
                    elif prev == "local":
                        self.refs.append((j, "decl0" if blocks == 0 else "decl", toks[j][1]))
                    else:
                        self.refs.append((j, "assign", toks[j][1]))
                    while toks[j][1] != "(":
                        j += 1
                stack.append("fn")
                j += 1
                while toks[j][1] != ")":
                    if toks[j][0] == "name":
                        self.depth_at[j] = (blocks + 1, True)
                        self.refs.append((j, "decl", toks[j][1]))
                    j += 1
                k = j + 1
                continue
            if text in KEYWORDS:
                k += 1
                continue
            if prev in (".", ":") or prev == "goto" or toks[k - 1][1] == "::":
                k += 1
                continue
            if stack and stack[-1] == "{" and nxt == "=":
                k += 1  # a table key
                continue
            if _local_list(toks, k):
                self.refs.append((k, "decl0" if blocks == 0 else "decl", text))
                k += 1
                continue
            lhs = _assigned(toks, k) and not (stack and stack[-1] in "{([")
            self.refs.append((k, "assign" if lhs else "use", text))
            k += 1
        assert not stack, f"unclosed blocks: {stack}"


def _local_list(toks, k):
    """Whether toks[k] is a name in a `local a, b, ...` list."""
    j = k - 1
    while j >= 1 and toks[j][1] == "," and toks[j - 1][0] == "name":
        j -= 2
    return j >= 0 and toks[j][1] == "local"


def _assigned(toks, k):
    """Whether toks[k] is a target of `a, b = ...` (bare names only)."""
    j = k + 1
    while toks[j][1] == "," and toks[j + 1][0] == "name":
        j += 2
    if toks[j][1] != "=":
        return False
    # the list must start the statement: walk back over `name ,`
    i = k - 1
    while i >= 1 and toks[i][1] == "," and toks[i - 1][0] == "name":
        i -= 2
    return i < 0 or toks[i][1] not in (".", ":", "[", "(", ",", "=")


def split(src, modules=None):
    modules = MODULES if modules is None else modules
    lines = src.split("\n")
    starts = []
    for anchor, path, what in modules:
        hits = [no for no, line in enumerate(lines) if re.match(anchor, line)]
        if len(hits) != 1:
            sys.exit(f"anchor {anchor!r} matched {len(hits)} lines, expected one")
        starts.append(hits[0])
    if starts != sorted(starts):
        sys.exit("anchors are out of file order")
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line) + 1)
    cut = [offsets[s] for s in starts] + [len(src)]
    names = [m[1] for m in modules]

    scan = Scan(src)
    toks = scan.toks

    def module_of(pos):
        for i in range(len(modules)):
            if cut[i] <= pos < cut[i + 1]:
                return i
        return -1  # the header

    # Every cut must fall between blocks.
    for i, pos in enumerate(cut[:-1]):
        k = next((t for t, tok in enumerate(toks) if tok[2] >= pos), len(toks))
        if k < len(toks) and scan.depth_at[k][0] != 0:
            sys.exit(f"{names[i]} starts inside a block")

    file_level = {}  # name -> module index
    nested = {}  # name -> count of nested declarations
    assigns = {}  # name -> [(module, in_fn)]
    uses = {}  # name -> {module}
    for k, kind, name in scan.refs:
        mod = module_of(toks[k][2])
        if kind == "decl0":
            file_level.setdefault(name, mod)
        elif kind == "decl":
            nested[name] = nested.get(name, 0) + 1
        elif kind == "assign":
            assigns.setdefault(name, []).append((mod, scan.depth_at[k][1]))
            uses.setdefault(name, set()).add(mod)
        else:
            uses.setdefault(name, set()).add(mod)

    late, aliased = set(), {}
    for name, home in file_level.items():
        later = {m for m in uses.get(name, ()) if m > home}
        if not later:
            continue
        rebound = [(m, f) for m, f in assigns.get(name, ()) if m != home or f]
        if rebound:
            if nested.get(name):
                sys.exit(f"{name} is assigned outside {names[home]} and also declared "
                         f"again elsewhere; rename one before splitting")
            late.add(name)
        else:
            aliased[name] = (home, later)

    # Rewrite late-bound names to shared.name, declarations included.
    edits = []  # (start, end, replacement)
    for k, kind, name in scan.refs:
        if name not in late:
            continue
        s, e = toks[k][2], toks[k][3]
        if kind == "decl0":
            edits.append(_drop_declaration(toks, k, src))
        else:
            edits.append((s, e, f"{SHARED}.{name}"))
    edits = [x for x in edits if x]
    out = src
    for s, e, text in sorted(edits, reverse=True):
        out = out[:s] + text + out[e:]
    # Cut offsets moved with the edits: recompute them from the anchors.
    lines = out.split("\n")
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line) + 1)
    starts = [next(no for no, line in enumerate(lines) if re.match(a, line)) for a, _, _ in modules]
    cut = [offsets[s] for s in starts] + [len(out)]

    header = out[:cut[0]]
    files = {}
    for i, (_, path, what) in enumerate(modules):
        body = out[cut[i]:cut[i + 1]].rstrip("\n") + "\n"
        imports = sorted(n for n, (home, later) in aliased.items() if i in later)
        exports = sorted(n for n, (home, later) in aliased.items() if home == i)
        uses_shared = imports or exports or any(
            module_of_out == i for module_of_out in _late_modules(out, cut, late))
        head = f"-- Highlander: {what}. Split from rules.lua by tools/split_highlander.py.\n"
        if uses_shared:
            head += f'local {SHARED} = require("lib/shared")\n'
        for n in imports:
            head += f"local {n} = {SHARED}.{n}\n"
        tail = "".join(f"{SHARED}.{n} = {n}\n" for n in exports)
        files[path + ".lua"] = head + "\n" + body + ("\n" + tail if tail else "")
    files["lib/shared.lua"] = (
        "-- Highlander: what its modules share, one table per match.\n"
        "-- Split from rules.lua by tools/split_highlander.py.\n"
        "return {}\n"
    )
    entry = header.rstrip("\n") + "\n\n" + (
        "-- The pack's code is split into modules, run in this order; together\n"
        "-- they are the pack. tools/split_highlander.py made them from the\n"
        "-- single rules.lua this file once was.\n"
    ) + "".join(f'require("{p}")\n' for _, p, _ in modules)
    files["rules.lua"] = entry
    return files, sorted(late), aliased


def _late_modules(src, cut, late):
    """Module indices whose text mentions shared.<late name>."""
    found = set()
    for i in range(len(cut) - 1):
        body = src[cut[i]:cut[i + 1]]
        if any(f"{SHARED}.{n}" in body for n in late):
            found.add(i)
    return found


def _drop_declaration(toks, k, src):
    """Remove a late-bound name from its file-level `local` statement."""
    name = toks[k][1]
    # find the `local` that starts the list
    j = k
    while toks[j][1] != "local":
        j -= 1
    listed = []
    m = j + 1
    while True:
        listed.append(m)
        if toks[m + 1][1] != ",":
            break
        m += 2
    has_value = m + 1 < len(toks) and toks[m + 1][1] == "="
    if len(listed) == 1:
        if has_value:  # local x = v  ->  shared.x = v
            return (toks[j][2], toks[k][3], f"{SHARED}.{name}")
        # local x  ->  (nothing: the line goes)
        line_start = src.rfind("\n", 0, toks[j][2]) + 1
        line_end = src.find("\n", toks[k][3])
        return (line_start, line_end + 1, "")
    if has_value:
        sys.exit(f"{name}: a late-bound name in a multi-name local with values; split it by hand")
    # local a, x, b  ->  local a, b
    i = listed.index(k)
    if i + 1 < len(listed):
        return (toks[k][2], toks[listed[i + 1]][2], "")
    return (toks[listed[i - 1]][3], toks[k][3], "")


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    src = Path(sys.argv[1]).read_text(encoding="utf-8")
    pack = Path(sys.argv[2])
    files, late, aliased = split(src)
    for rel, text in files.items():
        path = pack / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    print(f"{len(files)} files; late-bound via {SHARED}: {', '.join(late) or 'none'}; "
          f"shared aliases: {len(aliased)}")


if __name__ == "__main__":
    main()
