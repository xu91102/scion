"""Git boundary fault injection for check-workspaces.py."""
import os
from pathlib import Path
import subprocess
import sys
import time


git = os.environ["SCION_TEST_REAL_GIT"]
root = Path(os.environ["SCION_TEST_ROOT"])
mode = os.environ["SCION_TEST_GIT_MODE"]
args = sys.argv[1:]
if mode == "stale" and "ls-remote" in args:
    marker = root / "remote-read"
    if marker.exists():
        subprocess.run([git, "-C", str(root / "origin.git"), "update-ref",
                        "refs/heads/main", os.environ["SCION_TEST_NEW_HEAD"]], check=True)
    else:
        marker.touch()
if mode == "unsupported-sparse" and "sparse-checkout" in args:
    sys.exit(129)

result = subprocess.run([git, *args])
if mode == "interrupt" and result.returncode == 0 and "worktree" in args and "add" in args:
    (root / "worktree-created").touch()
    deadline = time.monotonic() + 20
    while not (root / "release-wrapper").exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    (root / "wrapper-exited").touch()
sys.exit(result.returncode)
