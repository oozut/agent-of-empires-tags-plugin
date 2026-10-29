"""Tests for the LLM Tags plugin. Stdlib ``unittest`` only: no aoe, no network.

Run from the plugin root:

    python3 -m unittest discover -s tests -t . -v

That command is the M0-T03 evidence: every test either passes without a daemon or
uses a fake ``aoe`` on ``PATH``/``AOE_BIN``.
"""

import sys
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src"
if str(SRC) not in sys.path:  # import llm_tags without installing it
    sys.path.insert(0, str(SRC))
