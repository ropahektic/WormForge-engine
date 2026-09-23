import contextlib
import importlib.util
import io
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location(
    "sync_state_diff", Path(__file__).with_name("sync-state-diff.py"))
diff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diff)


class SnapshotTests(unittest.TestCase):
    # Known rotate/add checksum for bytes 01 02 03 is ((1*8)+2)*8+3 = 83.
    snapshot = ("stock state f=411 bytes=3 stream-hash=00000053\n"
                "stock state data f=411 off=00000000 hex=010203\n"
                "stock state end f=411\n")

    def test_latest_session_only(self):
        log = self.snapshot + "wkLua build new\n" + self.snapshot.replace("411", "500")
        self.assertEqual(diff.snapshots(log), {500: b"\x01\x02\x03"})

    def test_latest_round_only(self):
        log = ("replay recorder: pending old.WF_WAgame\n" + self.snapshot
               + "replay recorder: pending new.WF_WAgame\n"
               + self.snapshot.replace("411", "600"))
        self.assertEqual(diff.snapshots(log), {600: b"\x01\x02\x03"})

    def test_missing_bytes_or_end_are_rejected(self):
        for text in (self.snapshot.replace("hex=010203", "hex=0102"),
                     self.snapshot.replace("stock state end f=411\n", ""),
                     self.snapshot.replace("off=00000000", "off=00000001"),
                     self.snapshot.replace("hex=010203", "hex=010204")):
            with self.subTest(text=text), self.assertRaises(ValueError):
                diff.snapshots(text)

    def test_length_difference_is_not_lost(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertTrue(diff.compare({411: b"abc"}, {411: b"abcd"}))
        self.assertIn("1 differing bytes", output.getvalue())

    def test_missing_snapshots_do_not_report_success(self):
        with self.assertRaises(ValueError):
            diff.compare({}, {})


if __name__ == "__main__":
    unittest.main()
