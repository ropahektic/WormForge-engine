#!/usr/bin/env python3
"""Check tools/split_highlander.py on small files: what it shares, rewrites
and refuses, and that the modules it writes run as the single file did."""

from pathlib import Path
import os
import subprocess
import tempfile
import unittest

from split_highlander import split

MODULES = [
    (r"^-- -+ one$", "a/one", "one"),
    (r"^-- -+ two$", "a/two", "two"),
]

MONOLITH = """-- header
-- ---------- one
local log = {}
local later
local function call_later(x) return later(x) end
local keys = { log = 1 }
local tbl = { field = keys.log }
-- ---------- two
log[#log + 1] = "two ran"
later = function(x) return x * 2 end
log[#log + 1] = call_later(21)
log[#log + 1] = tbl.field
"""


def run_pack(files, check):
    with tempfile.TemporaryDirectory() as temp:
        for rel, text in files.items():
            path = Path(temp) / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        code = """
local root = os.getenv("PACK") .. "/"
local modules = {}
function require(name)
  if modules[name] ~= nil then return modules[name] end
  local env = setmetatable({}, { __index = _G })
  local result = assert(loadfile(root .. name .. ".lua", "t", env))()
  if result == nil then result = true end
  modules[name] = result
  return result
end
dofile(root .. "rules.lua")
local shared = require("lib/shared")
""" + check
        subprocess.run(["lua", "-"], input=code, check=True, capture_output=True,
                       text=True, env={**os.environ, "PACK": temp})


class Split(unittest.TestCase):
    def test_shares_rewrites_and_runs_as_one_file(self):
        files, late, aliased = split(MONOLITH, MODULES)
        self.assertEqual(late, ["later"])
        self.assertEqual(sorted(aliased), ["call_later", "log", "tbl"])
        two = files["a/two.lua"]
        self.assertIn("local log = shared.log", two)
        self.assertIn("shared.later = function", two)
        self.assertNotIn("local keys", two, "keys is a table key and a field in two, not a use")
        one = files["a/one.lua"]
        self.assertNotIn("local later", one)
        self.assertIn("return shared.later(x)", one)
        self.assertEqual(files["rules.lua"].count("require("), 2)
        run_pack(files, """
assert(shared.log[1] == "two ran" and shared.log[2] == 42 and shared.log[3] == 1)
""")

    def test_refuses_a_late_bound_name_declared_again(self):
        src = MONOLITH.replace("local function call_later(x)",
                               "local function shadow(later) return later end\nlocal function call_later(x)")
        with self.assertRaises(SystemExit):
            split(src, MODULES)

    def test_refuses_a_missing_or_doubled_anchor(self):
        with self.assertRaises(SystemExit):
            split(MONOLITH.replace("-- ---------- two", "-- nothing"), MODULES)
        with self.assertRaises(SystemExit):
            split(MONOLITH + "-- ---------- two\n", MODULES)


if __name__ == "__main__":
    unittest.main()
