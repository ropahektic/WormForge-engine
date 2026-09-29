import contextlib
import importlib.util
import io
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location(
    "sync_input_matrix", Path(__file__).with_name("sync-input-matrix.py"))
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)


def complete_log(checksum="1234abcd", include_w=True, pointer_motion=True):
    samples = [(0, 10, 20, False)]
    for _, bit in matrix.KEYS:
        if bit == 0x10 and not include_w:
            continue
        samples.extend(((bit, 10, 20, False), (0, 10, 20, False)))
    samples.extend(((0, 10, 20, True),
                    (0, 11 if pointer_motion else 10, 20, True),
                    (0, 11, 20, False)))
    lines = ["wkLua build matrix-test", "replay recorder: pending test.WF_WAgame"]
    for direction in ("tx", "rx"):
        for frame, (keys, x, y, down) in enumerate(samples, 1):
            lines.append(
                f"sync input {direction} f={frame} keys={keys:#04x} "
                f"pointer={x},{y} down={'true' if down else 'false'}"
            )
    for component in matrix.COMPONENTS:
        lines.append(
            f"sync rx f=50 {component}=11111111 local=11111111 OK"
        )
    lines.extend((
        "Q equip bazooka ammo=1", "Q equip bazooka ammo=1",
        "space charge worm=0x1", "space charge worm=0x2",
        "space shoot worm=0x1", "space shoot worm=0x2",
        f"stock dispatch f=100 kind=0x08 size=8 data=00000064,{checksum},00000000 "
        f"expected-frame=100 expected-checksum={checksum} checksum-state=0 "
        "local-input=0 remote-input=1",
        "replay recorder: completed test.WF_WAgame",
    ))
    return "\n".join(lines) + "\n"


class MatrixTests(unittest.TestCase):
    def inspect(self, log):
        with contextlib.redirect_stdout(io.StringIO()):
            return matrix.inspect("test", log)

    def compare(self, left, right):
        with contextlib.redirect_stdout(io.StringIO()):
            return matrix.compare(left, right)

    def test_complete_matrix_passes(self):
        log = matrix.parse(complete_log())
        self.assertEqual(self.inspect(log), [])
        self.assertEqual(self.compare(log, matrix.parse(complete_log())), [])

    def test_current_recorder_line_starts_a_round(self):
        # Builds since the replay trailer log "recording", not "pending".
        log = complete_log().replace(
            "replay recorder: pending test.WF_WAgame",
            "replay recorder: recording test.WAgame (streams in x)")
        self.assertEqual(self.inspect(matrix.parse(log)), [])

    def test_missing_key_is_reported(self):
        failures = self.inspect(matrix.parse(complete_log(include_w=False)))
        self.assertTrue(any("W" in failure for failure in failures))

    def test_pointer_motion_is_required(self):
        failures = self.inspect(matrix.parse(complete_log(pointer_motion=False)))
        self.assertTrue(any("held motion" in failure for failure in failures))

    def test_stock_difference_is_reported(self):
        left = matrix.parse(complete_log())
        right = matrix.parse(complete_log(checksum="deadbeef"))
        self.assertTrue(any("sequences differ" in failure
                            for failure in self.compare(left, right)))

    def test_latest_launch_and_round_are_selected(self):
        stale = complete_log(checksum="deadbeef")
        current = complete_log()
        parsed = matrix.parse(stale + current)
        self.assertEqual(parsed.build, "matrix-test")
        self.assertEqual(parsed.stock, [(100, "1234abcd")])


if __name__ == "__main__":
    unittest.main()
