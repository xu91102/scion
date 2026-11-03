"""Exercise the released CLI with independent processes, real Git and SQLite."""
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time


BINARY = Path(sys.argv[1]).resolve()
LEGACY = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None
LEGACY_DATA = Path(sys.argv[3]).resolve() if len(sys.argv) > 3 else None


def check(root):
    source, remote, state = (root / name for name in ("source", "origin.git", "managed"))
    home = root / "home"
    home.mkdir()
    db = root / "state.sqlite"
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(HOME=str(home), GIT_CONFIG_NOSYSTEM="1", XDG_CONFIG_HOME=str(home))

    wrapper = root / "bin/git"
    wrapper.parent.mkdir()
    wrapper.write_text(f"#!{sys.executable}\n" + Path(__file__).with_name("workspace-git-fixture.py").read_text())
    wrapper.chmod(0o700)
    def fault(mode, **settings):
        return dict(env, PATH=str(wrapper.parent) + os.pathsep + env["PATH"],
                    SCION_TEST_REAL_GIT=shutil.which("git"), SCION_TEST_ROOT=str(root),
                    SCION_TEST_GIT_MODE=mode, **settings)

    def wait_for(marker):
        deadline = time.monotonic() + 15
        while not (root / marker).exists():
            assert time.monotonic() < deadline, marker
            time.sleep(0.01)

    def git(path, *args):
        command = ["git", "-C", str(path), "-c", "user.name=Fixture", "-c",
                   "user.email=fixture@example.invalid", *args]
        return subprocess.check_output(command, env=env, stderr=subprocess.PIPE).strip()

    def cli(*args, code=0, binary=BINARY, extra_env=None):
        result = subprocess.run([str(binary), *map(str, args)], env=extra_env or env,
                                capture_output=True, timeout=30)
        assert result.returncode == code, (args, result.returncode, result.stderr.decode(), result.stdout.decode())
        return json.loads(result.stdout or result.stderr)

    def spec(task, scope):
        path = root / (task + ".json")
        path.write_text(json.dumps(dict(id=task, repository="example/scion", target_branch="main",
            goal="fixture task", allowed_paths=[scope], acceptance_criteria=["preserve content"],
            owner="fixture", max_attempts=2, timeout_seconds=30)))
        return path

    def task(task_id, scope):
        cli("task-create", spec(task_id, scope), db)

    def parallel(*ids):
        processes = [subprocess.Popen([str(BINARY), "workspace-prepare", str(db), task_id],
                     env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for task_id in ids]
        results = []
        for process in processes:
            out, err = process.communicate(timeout=30)
            results.append((process.returncode, json.loads(out or err)))
        return results

    git(root, "init", "--bare", "--initial-branch=main", str(remote))
    git(root, "init", "--initial-branch=main", str(source))
    for directory in ("src/a", "src/ab", "docs"):
        (source / directory).mkdir(parents=True, exist_ok=True)
        (source / directory / "file.txt").write_text("original\n")
    git(source, "add", ".")
    git(source, "commit", "-m", "base")
    git(source, "push", str(remote), "main")
    git(source, "remote", "add", "origin", "https://github.com/example/scion.git")
    git(root, "config", "--global", f"url.{remote}.insteadOf", "https://github.com/example/scion.git")
    (source / "local.txt").write_text("keep user file\n")

    def source_state():
        return (git(source, "status", "--porcelain"), git(source, "show-ref"), git(source, "remote", "-v"),
                (source / ".git/index").read_bytes(), (source / "local.txt").read_text())

    original = source_state()
    configuration = root / "project.json"
    configuration.write_text(json.dumps(dict(repository="example/scion", source_checkout=str(source),
                                            state_root=str(state))))
    cli("project-register", configuration, source / "nested.sqlite", code=2)
    assert not (source / "nested.sqlite").exists()
    cli("project-register", configuration, db)
    task("left", "src/a")
    task("right", "src/ab")
    outcomes = parallel("left", "right")
    assert [code for code, _ in outcomes] == [0, 0], outcomes
    left, right = [value["result"]["workspace"] for _, value in outcomes]
    assert source_state() == original
    assert left == cli("workspace-prepare", db, "left")["result"]["workspace"]
    paths = [Path(w["path"]) for w in (left, right)]
    assert git(paths[0], "rev-parse", "--git-common-dir") == git(paths[1], "rev-parse", "--git-common-dir")
    assert git(paths[0], "rev-parse", "--git-path", "index") != git(paths[1], "rev-parse", "--git-path", "index")
    def edit(path, directory):
        (path / directory / "file.txt").write_text(f"change {directory}\n")
        git(path, "commit", "-am", "task change")

    with ThreadPoolExecutor(max_workers=2) as workers:
        list(workers.map(edit, paths, ("src/a", "src/ab")))
    heads = [git(path, "rev-parse", "HEAD") for path in paths]
    clean = cli("workspace-preflight", db, "left", "right")["result"]
    assert clean["outcome"] == "CLEAN" and clean["heads"] == [head.decode() for head in heads]
    assert [git(path, "rev-parse", "HEAD") for path in paths] == heads
    assert source_state() == original

    task("parent", "docs")
    task("child", "docs/sub")
    overlap = parallel("parent", "child")
    assert sorted(code for code, _ in overlap) == [0, 3], overlap
    blocker = next(value["result"] for code, value in overlap if code == 3)
    assert blocker["outcome"] == "BLOCKED" and {blocker["path"], blocker["occupied_path"]} == {"docs", "docs/sub"}

    (paths[0] / "dirty").write_text("preserved\n")
    assert cli("workspace-preflight", db, "left", code=3)["result"]["outcome"] == "DIRTY"
    cli("workspace-release", db, "left", "--confirm-stopped", code=2)
    (paths[0] / "dirty").unlink()
    tracked = paths[0] / "src/a/file.txt"
    content = tracked.read_bytes()
    git(paths[0], "update-index", "--assume-unchanged", "src/a/file.txt")
    tracked.write_bytes(b"hidden change\n")
    assert cli("workspace-preflight", db, "left", code=3)["result"]["outcome"] == "DIRTY"
    cli("workspace-release", db, "left", "--confirm-stopped", code=2)
    git(paths[0], "update-index", "--no-assume-unchanged", "src/a/file.txt")
    tracked.write_bytes(content)

    monitor = root / "monitor"
    monitor.write_text("#!/bin/sh\ntouch \"$SCION_TEST_MARKER\"\n")
    monitor.chmod(0o700)
    marker = root / "unexpected-script"
    git(paths[0], "config", "--local", "core.fsmonitor", str(monitor))
    assert not cli("workspace-show", db, "left", extra_env=dict(env, SCION_TEST_MARKER=str(marker)))["result"]["dirty"]
    assert not marker.exists()
    git(paths[0], "config", "--local", "--unset", "core.fsmonitor")
    for key in ("merge.probe.driver", "filter.probe.smudge"):
        git(paths[0], "config", "--local", key, str(monitor))
        result = cli("workspace-preflight", db, "left", code=2,
                     extra_env=dict(env, SCION_TEST_MARKER=str(marker)))
        assert "unsupported executable or transport" in result["error"] and not marker.exists()
        git(paths[0], "config", "--local", "--unset", key)

    git(paths[0], "mv", "src/a/file.txt", "docs/renamed.txt")
    git(paths[0], "commit", "-m", "out of scope rename")
    out = cli("workspace-preflight", db, "left", code=3)["result"]
    assert out["outside_scope"] == ["docs/renamed.txt"], out
    git(paths[0], "reset", "--hard", heads[0].decode())

    (paths[0] / "docs/binary").write_bytes(b"\x00\xff")
    git(paths[0], "add", "docs/binary")
    git(paths[0], "commit", "-m", "out of scope binary")
    assert cli("workspace-preflight", db, "left", code=3)["result"]["outside_scope"] == ["docs/binary"]
    outside_head = git(paths[0], "rev-parse", "HEAD").decode()
    git(paths[0], "replace", outside_head, heads[0].decode())
    git(paths[0], "reset", "--hard", "HEAD")
    assert cli("workspace-preflight", db, "left", code=3)["result"]["outcome"] == "DIRTY"
    git(paths[0], "--no-replace-objects", "reset", "--hard", "HEAD")
    assert cli("workspace-preflight", db, "left", code=3)["result"]["outside_scope"] == ["docs/binary"]
    git(paths[0], "replace", "-d", outside_head)
    git(paths[0], "reset", "--hard", heads[0].decode())
    git(paths[0], "update-index", "--add", "--cacheinfo", f"160000,{heads[0].decode()},src/a/nested")
    git(paths[0], "commit", "-m", "submodule entry")
    (paths[0] / "src/a/nested").mkdir()
    submodule = cli("workspace-preflight", db, "left", code=3)["result"]
    assert "submodule" in submodule.get("reason", ""), submodule
    nested = paths[0] / "src/a/nested"
    git(nested, "init")
    (nested / ".gitattributes").write_text("file filter=probe\n")
    (nested / "file").write_text("base\n")
    git(nested, "add", ".")
    git(nested, "commit", "-m", "nested fixture")
    git(nested, "config", "--local", "filter.probe.clean", f"{monitor}; cat")
    (nested / "file").write_text("edit\n")
    submodule = cli("workspace-preflight", db, "left", code=3,
                    extra_env=dict(env, SCION_TEST_MARKER=str(marker)))["result"]
    assert "initialized submodules" in submodule["reason"] and not marker.exists()
    shutil.rmtree(nested)
    git(paths[0], "reset", "--hard", heads[0].decode())

    external = root / "external"
    git(root, "clone", str(remote), str(external))
    (external / "src/a/file.txt").write_text("upstream edit\n")
    git(external, "commit", "-am", "upstream edit")
    git(external, "push", "origin", "main")
    conflict = cli("workspace-preflight", db, "right", "left", code=3)["result"]
    assert conflict["outcome"] == "CONFLICT" and "src/a/file.txt" in conflict["conflict_paths"], conflict
    assert conflict["target_sha"] != clean["target_sha"] and conflict["task"] == "left"
    assert source_state() == original

    current = git(external, "rev-parse", "HEAD").decode()
    stale_env = fault("stale", SCION_TEST_NEW_HEAD=clean["target_sha"])
    stale = cli("workspace-preflight", db, "left", "right", code=3, extra_env=stale_env)
    assert stale["result"]["outcome"] == "STALE", stale
    git(remote, "update-ref", "refs/heads/main", current)

    cli("workspace-release", db, "left", "--confirm-stopped")
    assert paths[0].is_dir() and git(paths[0], "rev-parse", "HEAD") == heads[0]
    assert cli("workspace-prepare", db, "left")["result"]["workspace"]["phase"] == "released"
    task("sparse", "src/a")
    sparse = cli("workspace-prepare", db, "sparse", "--sparse", "src/a")["result"]["workspace"]
    sparse_path = Path(sparse["path"])
    assert (sparse_path / "src/a/file.txt").is_file() and not (sparse_path / "src/ab/file.txt").exists()
    assert (paths[1] / "src/a/file.txt").is_file() and (paths[1] / "src/ab/file.txt").is_file()

    git(sparse_path, "rm", "src/a/file.txt")
    git(sparse_path, "commit", "-m", "delete file")
    git(external, "mv", "src/a/file.txt", "src/a/renamed.txt")
    git(external, "commit", "-m", "rename file")
    git(external, "push", "origin", "main")
    assert cli("workspace-preflight", db, "sparse", code=3)["result"]["outcome"] == "CONFLICT"

    task("unsupported", "other")
    cli("workspace-prepare", db, "unsupported", "--sparse", "src/ab", code=2,
        extra_env=fault("unsupported-sparse"))
    unsupported = cli("workspace-show", db, "unsupported")["result"]["workspace"]
    assert unsupported["phase"] == "preparing" and "sparse" in unsupported["last_error"]
    assert not (sparse_path / "src/ab/file.txt").exists()
    assert (paths[1] / "src/ab/file.txt").is_file()
    recovered = cli("workspace-recover", db)["result"]["results"][0]["workspace"]
    second_sparse = Path(recovered["path"])
    assert recovered["phase"] == "ready"
    assert (second_sparse / "src/ab/file.txt").is_file() and not (second_sparse / "src/a/renamed.txt").exists()
    git(second_sparse, "sparse-checkout", "set", "--cone", "src/a")
    assert not cli("workspace-show", db, "unsupported")["result"]["dirty"]
    assert (second_sparse / "src/a/renamed.txt").is_file()
    assert not (sparse_path / "src/ab/file.txt").exists() and (paths[1] / "src/ab/file.txt").is_file()

    task("interrupted", "interrupt")
    process = subprocess.Popen([str(BINARY), "workspace-prepare", str(db), "interrupted"],
                               env=fault("interrupt"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        wait_for("worktree-created")
        process.kill()
        process.wait(timeout=5)
    finally:
        (root / "release-wrapper").touch()
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
    wait_for("wrapper-exited")
    interrupted = cli("workspace-show", db, "interrupted")["result"]["workspace"]
    assert interrupted["phase"] == "preparing" and Path(interrupted["path"]).is_dir()
    resumed = cli("workspace-recover", db)["result"]["results"][0]["workspace"]
    assert resumed["path"] == interrupted["path"] and resumed["phase"] == "ready"

    task("reserved", "reserved")
    with sqlite3.connect(db) as sql:
        sql.execute("""CREATE TRIGGER stop_prepare BEFORE UPDATE ON workspaces
                       WHEN NEW.task='reserved' AND NEW.revision=1
                       BEGIN SELECT raise(ABORT, 'injected stop after reservation'); END""")
    cli("workspace-prepare", db, "reserved", code=2)
    reserved = cli("workspace-show", db, "reserved")["result"]["workspace"]
    assert reserved["phase"] == "reserved" and not Path(reserved["path"]).exists()
    with sqlite3.connect(db) as sql:
        sql.execute("DROP TRIGGER stop_prepare")
    assert cli("workspace-recover", db)["result"]["results"][0]["workspace"]["path"] == reserved["path"]

    task("recover", "extra")
    with sqlite3.connect(db) as sql:
        sql.execute("""CREATE TRIGGER stop_ready BEFORE UPDATE ON workspaces
                       WHEN NEW.task='recover' AND NEW.revision=2
                       BEGIN SELECT raise(ABORT, 'injected stop before ready'); END""")
    cli("workspace-prepare", db, "recover", code=2)
    before = cli("workspace-show", db, "recover")["result"]["workspace"]
    assert before["phase"] == "preparing" and Path(before["path"]).is_dir()
    with sqlite3.connect(db) as sql:
        sql.execute("DROP TRIGGER stop_ready")
    after = cli("workspace-recover", db)["result"]["results"][0]["workspace"]
    assert after["phase"] == "ready" and before["path"] == after["path"] and before["base"] == after["base"]

    if LEGACY:
        old = root / "legacy.sqlite"
        prior = cli("task-create", spec("legacy", "docs"), old, binary=LEGACY)
        cli("task-show", old, "legacy", code=2)
        upgraded = cli("state-upgrade", old)["result"]["backup"]
        assert cli("task-show", old, "legacy") == prior
        assert cli("task-show", upgraded, "legacy", binary=LEGACY) == prior
        assert cli("state-upgrade", old)["result"]["backup"] is None
        print("M1 binary migration and backup replay PASS")

    if LEGACY_DATA:
        old = root / "historical.sqlite"
        with sqlite3.connect(LEGACY_DATA) as previous, sqlite3.connect(old) as copy:
            previous.backup(copy)
        history = cli("history", old, "xu91102/scion", 1, binary=LEGACY)
        assert len(history["records"]) > 0
        with sqlite3.connect(old) as sql:
            sql.execute("CREATE TABLE preflights (injected_failure TEXT)")
        cli("state-upgrade", old, code=2)
        with sqlite3.connect(old) as sql:
            assert sql.execute("PRAGMA user_version").fetchone()[0] == 1
            assert not sql.execute("SELECT name FROM sqlite_master WHERE name='projects'").fetchone()
            sql.execute("DROP TABLE preflights")
        old.with_name(old.name + ".schema1-backup").rename(root / "failed-upgrade-backup")
        backup = cli("state-upgrade", old)["result"]["backup"]
        assert cli("history", old, "xu91102/scion", 1) == history
        assert cli("history", backup, "xu91102/scion", 1, binary=LEGACY) == history
        print("M1 historical observation, migration rollback and backup PASS")


with tempfile.TemporaryDirectory(prefix="scion-workspaces-") as temp:
    check(Path(temp).resolve())
print("Git/SQLite workspace subprocess scenarios PASS")
