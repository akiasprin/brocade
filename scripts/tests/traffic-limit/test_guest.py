import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from guest import read_json


class CheckpointReaderTests(unittest.TestCase):
    def test_regular_missing_corrupt_and_oversized_files(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'checkpoint.json'
            self.assertIsNone(read_json(path))
            path.write_text(json.dumps({'value': 3}))
            self.assertEqual(read_json(path), {'value': 3})
            path.write_text('{truncated')
            self.assertIsNone(read_json(path))
            path.write_bytes(b' ' * (128 * 1024 + 1))
            self.assertIsNone(read_json(path))
            self.assertIsNone(read_json(Path(directory)))

    def test_fifo_and_symlink_never_block_or_read_another_file(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            fifo = directory / 'fifo'
            os.mkfifo(fifo, 0o600)
            target = directory / 'target.json'
            target.write_text('{"private":true}')
            link = directory / 'link'
            link.symlink_to(target)
            code = 'import sys; from pathlib import Path; from guest import read_json; assert read_json(Path(sys.argv[1])) is None'
            for path in [fifo, link]:
                # A regression to blocking open must fail this test within a fixed deadline.
                subprocess.run([sys.executable, '-c', code, str(path)],
                               cwd=Path(__file__).parent, timeout=3, check=True,
                               capture_output=True)
            self.assertEqual(target.read_text(), '{"private":true}')


if __name__ == '__main__':
    unittest.main()
