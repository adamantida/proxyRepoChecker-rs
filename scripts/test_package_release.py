import json
import unittest
from unittest.mock import patch

import package_release


class PackageVersionTests(unittest.TestCase):
    @patch("package_release.subprocess.run")
    def test_cargo_metadata_is_decoded_as_utf8(self, run):
        run.return_value.stdout = json.dumps(
            {"packages": [{"name": "proxy-rs", "version": "0.1.0", "description": "代理"}]}
        )

        self.assertEqual(package_release.package_version(), "0.1.0")
        self.assertEqual(run.call_args.kwargs["encoding"], "utf-8")


if __name__ == "__main__":
    unittest.main()
