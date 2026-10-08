#!/usr/bin/env python3
"""Check pack simulation math for operations with platform-dependent rounding.

Conservative: unknown/exported functions are simulation. Functions referenced
only by draw/HUD callbacks (including their local helpers) may use CRT maths.
Strings and comments are ignored; Lua's ^ operator is checked separately.
"""
from __future__ import annotations
import argparse
from dataclasses import dataclass, field
from pathlib import Path
import re

UNSAFE = {'sin', 'cos', 'tan', 'asin', 'acos', 'atan', 'exp', 'log', 'pow'}
TOKEN = re.compile(r'--\[(=*)\[[\s\S]*?\]\1\]|--[^\n]*|\[(=*)\[[\s\S]*?\]\2\]|"(?:\\[\s\S]|[^"\\])*"|\'(?:\\[\s\S]|[^\'\\])*\'|[A-Za-z_]\w*|\d+(?:\.\d+)?|[^\s]')

@dataclass
class Scope:
    name: str
    parent: int
    render: bool = False
    refs: set[str] = field(default_factory=set)
    violations: list[tuple[int, str]] = field(default_factory=list)


def check(source: str) -> list[tuple[int, str]]:
    tokens = [(m[0], source.count('\n', 0, m.start()) + 1) for m in TOKEN.finditer(source)
              if not m[0].startswith(('--', '"', "'", '[')) or m[0] == '[']
    scopes = [Scope('<module>', 0)]
    stack: list[tuple[str, int]] = []
    owners: dict[str, set[int]] = {}
    current = 0
    pending_do = 0
    for i, (t, line) in enumerate(tokens):
        if t == 'function':
            j = i+1
            name = ''
            while j < len(tokens) and tokens[j][0] != '(':
                name += tokens[j][0]; j += 1
            before = [x[0] for x in tokens[max(0, i-8):i]]
            render = any(''.join(before[-n:]) in ('wa.on.draw(', 'wa.on.hud(') for n in range(1, len(before)+1))
            scopes.append(Scope(name or f'<callback:{line}>', current, render))
            child = len(scopes)-1
            if name: owners.setdefault(name.split('.')[-1].split(':')[-1], set()).add(child)
            else:
                # An anonymous callback not registered for rendering is simulation.
                scopes[current].refs.add(scopes[child].name)
                owners.setdefault(scopes[child].name, set()).add(child)
            stack.append(('function', current)); current = child
        elif t in ('if', 'for', 'while', 'repeat'):
            stack.append((t, current))
            if t in ('for', 'while'): pending_do += 1
        elif t == 'do':
            if pending_do: pending_do -= 1
            else: stack.append(('do', current))
        elif t in ('end', 'until'):
            if stack:
                kind, parent = stack.pop()
                if kind == 'function': current = parent
        else:
            if re.fullmatch(r'[A-Za-z_]\w*', t): scopes[current].refs.add(t)
            if t == '^': scopes[current].violations.append((line, '^ calls platform pow; use multiplication'))
            if t == 'math' and i+2 < len(tokens) and tokens[i+1][0] == '.' and tokens[i+2][0] in UNSAFE:
                if i < 2 or tokens[i-2][0] != 'wa' or tokens[i-1][0] != '.':
                    scopes[current].violations.append((line, f'math.{tokens[i+2][0]} is platform dependent; use wa.math or simulation arithmetic'))
    render_roots = {i for i,s in enumerate(scopes) if s.render}
    def reachable(roots):
        found = set(roots); queue = list(roots)
        while queue:
            for ref in scopes[queue.pop()].refs:
                for child in owners.get(ref, ()):
                    if child not in found and child not in render_roots:
                        found.add(child); queue.append(child)
        return found
    rendering = reachable(render_roots)
    simulation = reachable({0} | {i for i in range(1,len(scopes)) if i not in rendering})
    return sorted(set(v for i,s in enumerate(scopes) if i in simulation for v in s.violations))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('paths', nargs='*', type=Path, default=[Path('mods/examples')])
    args = parser.parse_args()
    failed = False
    for path in args.paths:
        files = [path] if path.is_file() else sorted(path.rglob('*.lua'))
        for file in files:
            if file.name.startswith('test_'): continue  # host-only mocks, not shipped callbacks
            for line, message in check(file.read_text()):
                print(f'{file}:{line}: {message}'); failed = True
    if not failed: print('Pack simulation math: passed')
    return int(failed)

if __name__ == '__main__': raise SystemExit(main())
