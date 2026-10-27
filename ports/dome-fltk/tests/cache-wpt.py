#!/usr/bin/env python3
"""Compatibility entry point for the focused native Cache WPT suite."""
from pathlib import Path
import runpy
runpy.run_path(str(Path(__file__).with_name('native-wpt.py')), run_name='__main__')
