#!/usr/bin/env python3
"""Quick position check — delegates to live_pipeline.py monitor."""
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
sys.exit(subprocess.call(
    [sys.executable, str(root / "scripts" / "live_pipeline.py"), "monitor"],
    cwd=str(root),
))
