"""Tests of setup_model.py: python3 -m unittest test_setup_model (no GTK)."""
import os
import socket
import tempfile
import threading
import unittest
from pathlib import Path

import setup_model as model


class Answers(unittest.TestCase):
    def test_disks(self):
        self.assertEqual(model.parse_disks("disks /dev/nvme0n1=512110190592 /dev/sda=64000000000"),
                         [("/dev/nvme0n1", 512110190592), ("/dev/sda", 64000000000)])
        self.assertEqual(model.parse_disks("disks"), [])
        self.assertEqual(model.parse_disks("unavailable"), [])

    def test_systems(self):
        systems = model.parse_systems("systems /dev/vda=20000000000,2.2.0,2.1.0 /dev/sdb=1,2026.11.0,-")
        self.assertEqual(systems[0], {"device": "/dev/vda", "bytes": 20000000000, "release": "2.2.0", "previous": "2.1.0"})
        self.assertIsNone(systems[1]["previous"])
        self.assertEqual(model.parse_systems("unavailable"), [])

    def test_status(self):
        self.assertEqual(model.parse_status("running 45 Writing the recovery system"), ("running", 45, "Writing the recovery system"))
        self.assertEqual(model.parse_status("done"), ("done", 100, ""))
        self.assertEqual(model.parse_status("failed the disk is too small"), ("failed", 0, "the disk is too small"))
        self.assertEqual(model.parse_status("unavailable")[0], "failed")

    def test_sizes_and_releases(self):
        self.assertEqual(model.size_text(512110190592), "512 GB")
        self.assertEqual(model.size_text(1_000_204_886_016), "1.0 TB")
        self.assertEqual(model.compare_releases("2.10.0", "2.9.1"), 1)
        self.assertEqual(model.compare_releases("2026.11.0", "2026.11.0"), 0)
        self.assertEqual(model.compare_releases("2.1.0", "2.1.0-rc1"), -1)
        self.assertTrue(model.valid_name("Ana Souza"))
        self.assertFalse(model.valid_name("a:b"))
        self.assertFalse(model.valid_name("   "))
        self.assertTrue(model.valid_password("two words"))
        self.assertFalse(model.valid_password("tab\there"))


class ImageData(unittest.TestCase):
    def test_zones_and_layouts(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            (folder / "zone1970.tab").write_text("# comment\nBR\t-2332-04637\tAmerica/Sao_Paulo\nPT\t+3843-00908\tEurope/Lisbon,Atlantic/Madeira\n"
                                                 "AR\t-3436-05827\tAmerica/Argentina/Buenos_Aires\n")
            zones = model.time_zones(folder)
            self.assertEqual(zones["America"], ["Argentina/Buenos_Aires", "Sao_Paulo"])
            self.assertIn("Europe", zones)
            (folder / "base.lst").write_text("! model\n  pc105  Generic 105-key PC\n\n! layout\n  us   English (US)\n  br   Portuguese (Brazil)\n\n"
                                             "! variant\n  abnt2   br: Portuguese (Brazil, ABNT2)\n  intl    us: English (US, intl., with dead keys)\n")
            layouts, variants = model.keyboard_layouts(folder / "base.lst")
            self.assertEqual(layouts, [("us", "English (US)"), ("br", "Portuguese (Brazil)")])
            self.assertEqual(variants["br"], [("abnt2", "Portuguese (Brazil, ABNT2)")])
            self.assertEqual(model.keyboard_layouts(folder / "none.lst"), ([], {}))

    def test_disk_kinds(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            for name, removable in (("sda", "0"), ("sdb", "1")):
                (folder / name).mkdir()
                (folder / name / "removable").write_text(removable + "\n")
            self.assertEqual(model.disk_kind("/dev/nvme0n1", folder), "NVMe")
            self.assertEqual(model.disk_kind("/dev/vda", folder), "VirtIO")
            self.assertEqual(model.disk_kind("/dev/sda", folder), "SATA")
            self.assertEqual(model.disk_kind("/dev/sdb", folder), "USB")
            self.assertEqual(model.disk_kind("/dev/sdc", folder), "")


class SupervisorSocket(unittest.TestCase):
    def test_a_line_each_way(self):
        with tempfile.TemporaryDirectory() as folder:
            path = os.path.join(folder, "system.sock")
            server = socket.socket(socket.AF_UNIX)
            server.bind(path)
            server.listen(1)

            def answer():
                connection, _ = server.accept()
                line = connection.makefile().readline()
                connection.sendall(("got " + line).encode())
                connection.close()
            threading.Thread(target=answer, daemon=True).start()
            self.assertEqual(model.Supervisor(path).ask("install-list"), "got install-list")
            server.close()
            with self.assertRaises(ValueError):
                model.Supervisor(path).ask("x" * 400)


if __name__ == "__main__":
    unittest.main()
