#!/usr/bin/env python3
import unittest
from pathlib import Path
from check_sim_math import check

class CheckSimulationMath(unittest.TestCase):
    def test_shipped_packs(self):
        root = Path(__file__).resolve().parent.parent / 'mods/examples'
        for path in sorted(root.rglob('*.lua')):
            if not path.name.startswith('test_'):
                with self.subTest(pack=str(path.relative_to(root))):
                    self.assertEqual(check(path.read_text()), [])
    def test_simulation_and_aliases_are_flagged(self):
        self.assertEqual(len(check('wa.on.turn_start(function() return math.sin(1) + x ^ 2 end)')), 2)
        self.assertEqual(len(check('local sine = math.sin; wa.on.turn_start(function() return sine(1) end)')), 1)
    def test_render_and_hud_are_allowed(self):
        self.assertEqual(check('wa.on.draw(function() return math.sin(x) ^ 2 end)\nwa.on.hud(function() return math.log(2) end)'), [])
    def test_helper_used_by_simulation_is_flagged(self):
        src = 'local function wave(x) return math.sin(x) end\nwa.on.draw(function() wave(1) end)'
        self.assertEqual(check(src), [])
        self.assertTrue(check(src+'\nwa.on.turn_start(function() wave(1) end)'))
        self.assertTrue(check(src+'\nwave(1)'))
    def test_literals_and_comments_are_ignored(self):
        self.assertEqual(check('-- math.sin(x)^2\nlocal s="math.cos ^"\nlocal p=[=[math.exp ^]=]\nreturn wa.math.sin(1)*wa.math.cos(1)'), [])
    def test_nested_blocks_and_deterministic_math(self):
        self.assertEqual(check('wa.on.draw(function() for i=1,2 do if i>0 then math.sin(i) end end end)\nwa.on.turn_start(function() wa.math.atan(1,2) end)'), [])
        self.assertTrue(check('local function f() while true do do math.exp(1) end end end'))

if __name__ == '__main__': unittest.main()
