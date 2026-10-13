from __future__ import annotations

import unittest
from unittest.mock import patch

from qianxing_bridge import native


class NativeResolutionTests(unittest.TestCase):
    def test_bundled_extension_wins_over_a_different_top_level_install(self) -> None:
        bundled = object()
        with patch.object(native, "import_module", return_value=bundled) as load:
            self.assertIs(native._extension(), bundled)
        load.assert_called_once_with("qianxing_bridge._qianxing_native")
