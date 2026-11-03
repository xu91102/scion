"""Exercise the CLI against actual Docker and Git with a deterministic fixture image."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

BINARY = Path(sys.argv[1]).resolve()
IMAGE = sys.argv[2]
VOLUME_IMAGE = sys.argv[3] if len(sys.argv) > 3 else None


def check(root):
    source = root / "source"
    remote = root / "origin.git"
    db = root / "state.sqlite"
    managed = root / "managed"
    home = root / "home"
    home.mkdir()
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("GIT_") and key != "SCION_AGENT_AUTH_FILE"}
    # Keep Docker Desktop's active context while isolating the Git configuration.
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home), GIT_CONFIG_NOSYSTEM="1",
               DOCKER_CONFIG=os.environ.get("DOCKER_CONFIG", str(Path.home() / ".docker")))

    def git(*args):
        return subprocess.check_output(["git", "-c", "user.name=Fixture",
            "-c", "user.email=fixture@example.invalid", *map(str, args)], env=env, stderr=subprocess.PIPE)

    def cli(*args, code=0, auth=None):
        command_env = env if auth is None else dict(env, SCION_AGENT_AUTH_FILE=str(auth))
        result = subprocess.run([str(BINARY), *map(str, args)], env=command_env,
                                capture_output=True, timeout=45)
        out = json.loads(result.stdout or result.stderr)
        assert result.returncode == code, (args, result.returncode, out)
        return out.get("result", out)

    git("init", "--initial-branch=main", source)
    (source / "tasks").mkdir()
    (source / "tasks/base.txt").write_text("base\n")
    git("-C", source, "add", ".")
    git("-C", source, "commit", "-m", "base")
    git("clone", "--bare", source, remote)
    url = "https://github.com/example/scion.git"
    git("-C", source, "remote", "add", "origin", url)
    git("config", "--global", f"url.{remote}.insteadOf", url)

    project = root / "project.json"
    project.write_text(json.dumps(dict(repository="example/scion", source_checkout=str(source),
                                       state_root=str(managed))))
    cli("project-register", project, db)

    def prepare(task, seconds, attempts=1):
        spec = root / (task + ".json")
        spec.write_text(json.dumps(dict(id=task, repository="example/scion", target_branch="main",
            goal=task, allowed_paths=["tasks/" + task], acceptance_criteria=["fixture result"],
            owner="fixture", max_attempts=attempts, timeout_seconds=seconds)))
        cli("task-create", spec, db)
        return cli("workspace-prepare", db, task)["workspace"]

    def containers(key):
        return subprocess.check_output(["docker", "container", "ls", "-q", "-a",
            "--filter", f"label=io.scion.attempt={key}"], env=env).strip()

    if VOLUME_IMAGE:
        prepare("volume", 15)
        rejected = cli("task-run", db, "volume", VOLUME_IMAGE, code=2)
        assert "writable Docker volumes" in rejected["error"]
        assert cli("task-attempts", db, "volume")["attempts"] == []

    good = prepare("success", 15)
    successful = cli("task-run", db, "success", IMAGE)["attempt"]
    assert successful["phase"] == "committed" and successful["changed_paths"] == ["tasks/success"]
    assert cli("task-run", db, "success", IMAGE)["attempt"] == successful
    cache = managed / "objects.git"
    assert git("--git-dir", cache, "show", successful["head"] + ":tasks/success") == b"fixture-ok\n"
    assert git("-C", good["path"], "status", "--porcelain") == b""
    assert not containers(successful["key"])

    prepare("outside", 15, attempts=2)
    for number in (1, 2):
        failure = cli("task-run", db, "outside", IMAGE, code=3)["attempt"]
        assert failure["phase"] == "failed" and failure["number"] == number
        assert "scope" in failure["reason"] and failure["head"] is None
        assert not containers(failure["key"])
    cli("task-run", db, "outside", IMAGE, code=2)
    assert len(cli("task-attempts", db, "outside")["attempts"]) == 2

    prepare("quota", 15)
    quota = cli("task-run", db, "quota", IMAGE, code=3)["attempt"]
    assert quota["phase"] == "failed" and quota["head"] is None
    assert not (managed / ("attempt-" + quota["key"]) / "work/tasks/quota").exists()
    assert not containers(quota["key"])

    prepare("credential", 15)
    credentials = root / "model-auth.json"
    credentials.write_text(json.dumps({"token": "fixture-secret-for-scion-test"}))
    leaked = cli("task-run", db, "credential", IMAGE, code=3, auth=credentials)["attempt"]
    assert leaked["phase"] == "failed" and leaked["credential_mounted"] and leaked["head"] is None
    assert "output contains model credentials" in leaked["reason"]
    assert not (managed / ("attempt-" + leaked["key"]) / "output/tasks/credential").exists()
    assert not containers(leaked["key"])

    prepare("timeout", 4)
    timed = cli("task-run", db, "timeout", IMAGE, code=3)["attempt"]
    assert timed["phase"] == "timed_out" and timed["head"] is None
    assert not containers(timed["key"])

    def start_and_wait(task):
        process = subprocess.Popen([str(BINARY), "task-run", str(db), task, IMAGE],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        deadline = time.monotonic() + 30
        while process.poll() is None and time.monotonic() < deadline:
            rows = cli("task-attempts", db, task)["attempts"]
            if rows:
                container = containers(rows[-1]["key"]).decode()
                if container and subprocess.run(["docker", "exec", container, "test", "-f", "/work/started"],
                        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
                    return process, rows[-1]["key"]
            time.sleep(.1)
        process.kill()
        out, err = process.communicate(timeout=10)
        raise AssertionError((task, out, err))

    prepare("cancel", 30)
    process, key = start_and_wait("cancel")
    request = cli("task-cancel", db, "cancel")["attempt"]
    assert request["phase"] in ("cancel_requested", "cancelled")
    out, err = process.communicate(timeout=20)
    assert process.returncode == 3, (out, err)
    assert cli("task-attempts", db, "cancel")["attempts"][-1]["phase"] == "cancelled"
    assert not containers(key)

    prepare("recover", 30)
    process, key = start_and_wait("recover")
    process.kill()
    process.communicate(timeout=10)
    recovered = cli("task-cancel", db, "recover")["attempt"]
    assert recovered["phase"] == "cancelled" and not containers(key)
    print("Docker fixture: commit, scope rejection, runtime quota, credential detection, attempt limit, timeout, live cancel and orphan cleanup PASS")


with tempfile.TemporaryDirectory(prefix="scion-agent-") as temp:
    check(Path(temp).resolve())
