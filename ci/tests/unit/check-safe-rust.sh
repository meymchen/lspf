#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
bash ci/check-safe-rust.sh

python3 - "$BASH" <<'PY'
import pathlib
import subprocess
import tempfile
import sys

root = pathlib.Path.cwd()
scratch = root / ".scratch"
scratch.mkdir(exist_ok=True)
fixture = pathlib.Path(tempfile.mkdtemp(prefix="safe-rust-", dir=scratch))
(fixture / "crates/lspf/src").mkdir(parents=True)
manifest = fixture / "crates/lspf/Cargo.toml"
lib = fixture / "crates/lspf/src/lib.rs"
workspace = fixture / "Cargo.toml"
for level, attribute in [("deny", "forbid"), ("allow", "forbid"), (None, "forbid"),
                         ("forbid", "deny"), ("forbid", None)]:
    manifest.write_text('[lints]\nworkspace = true\n')
    workspace.write_text(f'[workspace.lints.rust]\nunsafe_code = "{level}"\n' if level else '')
    lib.write_text(f'#![{attribute}(unsafe_code)]\n' if attribute else '')
    result = subprocess.run([sys.argv[1], "ci/check-safe-rust.sh", fixture.as_posix()],
                            capture_output=True, text=True)
    assert result.returncode != 0, (level, attribute)
    assert "must" in result.stderr, result.stderr
workspace.write_text('[workspace.lints.rust]\nunsafe_code = "forbid"\n')
lib.write_text('#![forbid(unsafe_code)]\n')
for inheritance in ['', '[lints]\nworkspace = false\n']:
    manifest.write_text(inheritance)
    result = subprocess.run([sys.argv[1], "ci/check-safe-rust.sh", fixture.as_posix()],
                            capture_output=True, text=True)
    assert result.returncode != 0 and "must inherit" in result.stderr, result.stderr
print("safe Rust policy rejects removed or weakened constraints")
PY
