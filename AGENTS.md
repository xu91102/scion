# Scion development

- Stack: Rust 1.98.0, Cargo workspace. GitHub/Git owns code, refs, PRs and CI results.
- Current milestone: architecture and offline assessment foundation. Keep implementation status explicit.
- `scion-core` owns data contracts; `scion-orchestrator` owns pure decisions; `scion-cli` owns input/output.
- No core dependency on HTTP, a model provider, a process runner or GitHub credentials.
- Only a future trusted merge controller may merge, after re-reading authoritative GitHub state.
- Caller-provided JSON and example evidence must never be treated as authenticated acceptance.
- Use independent branches/worktrees. Keep changes scoped and retain evidence for the tested revision.
- Checks: `scripts/check.sh` (format, clippy, tests, build, rustdoc and CLI smoke).
- Add a failing behavior test before implementing a new decision or CLI behavior.
- Never commit credentials, `.env`, caches, generated binaries or local workflow artifacts.
- Review and test all changes before pushing. Merge and release require separate authorization.
