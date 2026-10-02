"""Report how Highlander's rules.lua hangs together, section by section.

    python3 tools/highlander_sections.py [path/to/rules.lua]

rules.lua is cut into sections by its `-- ----...---- name` marker lines.
For each section the report lists its size, the file-level names it declares
(a `local` outside every block), the file-level names it uses that another
section declares, and the pack files it names. Then the pack files nothing
names. It reads the file only; the observable behaviour is pinned separately
by test_rules.lua's baseline.txt.

Names are found by a Lua tokenizer with block depth (function/do/if/repeat
open, end/until close), so a `local` inside a section's own `do ... end` is
private to it however it is indented. Uses are identifier tokens that are not
field names (after `.` or `:`) or table keys (`name =` inside `{ }`), so the
report can still overcount a name that is also a local elsewhere.
"""

import os
import re
import sys

DEFAULT = "mods/examples/highlander/rules.lua"
MARKER = re.compile(r"^-- -{8,} (.+)$")
OPENERS = {"function", "do", "if", "repeat"}
CLOSERS = {"end", "until"}
KEYWORDS = {
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function",
    "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return",
    "then", "true", "until", "while",
}


def tokens(src):
    """(kind, text, line) for names, strings and punctuation; no comments."""
    i, line, n = 0, 1, len(src)
    long_open = re.compile(r"\[(=*)\[")
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
        elif c.isspace():
            i += 1
        elif src.startswith("--", i):
            m = long_open.match(src, i + 2)
            if m:
                close = "]" + m.group(1) + "]"
                j = src.find(close, m.end())
                j = n if j < 0 else j + len(close)
            else:
                j = src.find("\n", i)
                j = n if j < 0 else j
            line += src.count("\n", i, j)
            i = j
        elif c in "\"'":
            j = i + 1
            while j < n and src[j] != c:
                j += 2 if src[j] == "\\" else 1
            yield ("str", src[i + 1:j], line)
            line += src.count("\n", i, j)
            i = j + 1
        elif long_open.match(src, i):
            m = long_open.match(src, i)
            close = "]" + m.group(1) + "]"
            j = src.find(close, m.end())
            j = n if j < 0 else j
            yield ("str", src[m.end():j], line)
            line += src.count("\n", i, j)
            i = j + len(close)
        elif c.isalpha() or c == "_":
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            yield ("name", src[i:j], line)
            i = j
        elif c.isdigit():
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] in "._"):
                j += 1
            i = j
        else:
            yield ("op", c, line)
            i += 1


def sections(lines):
    """[(name, first_line, last_line)], 1-based; the preamble is "(top)"."""
    out, start, name = [], 1, "(top)"
    for no, text in enumerate(lines, 1):
        m = MARKER.match(text)
        if m:
            if no > start:
                out.append((name, start, no - 1))
            start, name = no, m.group(1).strip()
    out.append((name, start, len(lines)))
    return out


def analyse(src):
    lines = src.split("\n")
    secs = sections(lines)

    def section_of(line):
        for name, a, b in secs:
            if a <= line <= b:
                return name
        return secs[-1][0]

    toks = list(tokens(src))
    depth = 0
    declared = {}  # file-level name -> section
    uses = {}  # section -> {name}
    braces = 0
    for k, (kind, text, line) in enumerate(toks):
        prev = toks[k - 1] if k else ("op", "", 0)
        nxt = toks[k + 1] if k + 1 < len(toks) else ("op", "", 0)
        if kind == "op":
            braces += {"{": 1, "}": -1}.get(text, 0)
            continue
        if kind != "name":
            continue
        if text in OPENERS:
            depth += 1
            continue
        if text in CLOSERS:
            depth -= 1
            continue
        if text in KEYWORDS:
            continue
        if prev == ("name", "local", prev[2]) or (
            prev[1] == "," and depth == 0 and _in_local_list(toks, k)
        ):
            if depth == 0:
                declared.setdefault(text, section_of(line))
            continue
        if prev[1] == "function" and prev[0] == "name" and k >= 2 and toks[k - 2][1] == "local":
            if depth == 1:  # `local function f` opened the function's own block
                declared.setdefault(text, section_of(line))
            continue
        if prev[1] in (".", ":"):
            continue
        if braces > 0 and nxt[1] == "=" and (k + 2 >= len(toks) or toks[k + 2][1] != "="):
            continue  # a table key
        uses.setdefault(section_of(line), set()).add(text)

    assets = {}
    for kind, text, line in toks:
        if kind == "str" and re.match(r"^(sprites|sounds|weps)/", text):
            assets.setdefault(section_of(line), set()).add(text)
    return secs, declared, uses, assets


def _in_local_list(toks, k):
    """Whether toks[k] continues a `local a, b, ...` name list."""
    j = k - 1
    while j >= 1 and toks[j][1] == "," and toks[j - 1][0] == "name":
        j -= 2
    return j >= 0 and toks[j][1] == "local"


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT
    src = open(path, encoding="utf-8").read()
    pack = os.path.dirname(path)
    secs, declared, uses, assets = analyse(src)

    print(f"# {path}: {len(src.splitlines())} lines, {len(secs)} sections, "
          f"{len(declared)} file-level names\n")
    for name, a, b in secs:
        own = sorted(n for n, s in declared.items() if s == name)
        borrowed = sorted(n for n in uses.get(name, ()) if n in declared and declared[n] != name)
        print(f"## {name}  (lines {a}-{b}, {b - a + 1})")
        if own:
            print(f"declares: {', '.join(own)}")
        if borrowed:
            by = {}
            for n in borrowed:
                by.setdefault(declared[n], []).append(n)
            for src_sec in sorted(by):
                print(f"uses from {src_sec}: {', '.join(by[src_sec])}")
        named = sorted(assets.get(name, ()))
        if named:
            print(f"names {len(named)} pack files")
        print()

    named = set().union(*assets.values()) if assets else set()
    missing = sorted(p for p in named if not _exists(pack, p))
    on_disk = set()
    for top in ("sprites", "sounds", "weps"):
        root = os.path.join(pack, top)
        for dirpath, dirnames, files in os.walk(root):
            rel = os.path.relpath(dirpath, pack)
            if any(f.endswith((".png", ".gif")) for f in files) and re.search(r"/frame\d|^sprites/", rel):
                if any(re.match(r"frame\d+\.png$", f) for f in files):
                    on_disk.add(rel)
                    dirnames[:] = []
                    continue
            for f in files:
                on_disk.add(os.path.join(rel, f))
    unnamed = sorted(p for p in on_disk if not any(_names(n, p) for n in named))
    print(f"# pack files: {len(named)} named, {len(missing)} named but missing, "
          f"{len(unnamed)} on disk and never named")
    for p in missing:
        print(f"missing: {p}")
    for p in unnamed:
        print(f"never named: {p}")


def _exists(pack, rel):
    full = os.path.join(pack, rel)
    return os.path.exists(full) or os.path.exists(full + ".png") or os.path.isdir(full)


def _names(named, path):
    """Whether the name `named` (with or without its extension) covers `path`."""
    stem = os.path.splitext(path)[0]
    return named in (path, stem) or path.startswith(named + "/")


if __name__ == "__main__":
    main()
