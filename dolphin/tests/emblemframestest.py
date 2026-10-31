"""Regeneration keeps every static emblem and supplies only rotated syncing glyphs."""
import pathlib
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET


class EmblemFramesTest(unittest.TestCase):
    def test_frames_match_generator_and_preserve_the_background(self):
        source = pathlib.Path(sys.argv[1])
        with tempfile.TemporaryDirectory() as output:
            subprocess.run([sys.executable, str(source / "generate.py"), output], check=True)
            for size in (8, 16, 22, 32):
                generated = pathlib.Path(output) / str(size)
                self.assertEqual(len(list(generated.glob("*.svg"))), 12)
                for svg in generated.glob("*.svg"):
                    self.assertEqual(svg.read_bytes(), (source / str(size) / svg.name).read_bytes())
                static = ET.parse(generated / "konedrive-syncing.svg").getroot()
                for frame in range(1, 8):
                    root = ET.parse(generated / f"konedrive-syncing-{frame}.svg").getroot()
                    self.assertEqual(root.attrib, static.attrib)
                    self.assertEqual([(e.tag, e.attrib) for e in root[:2]], [(e.tag, e.attrib) for e in static[:2]])
                    group = root[2]
                    self.assertTrue(group.tag.endswith("}g"))
                    self.assertEqual(group.attrib["transform"], f"rotate({frame * 22.5:g} {size / 2:g} {size / 2:g})")
                    self.assertEqual([(e.tag, e.attrib) for e in group], [(e.tag, e.attrib) for e in static[2:]])


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0]])
