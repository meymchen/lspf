#!/usr/bin/env bash
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

python3 - "$root" <<'PY'
import pathlib
import re
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest = tomllib.loads((root / "crates/lspf/Cargo.toml").read_text())
workspace = tomllib.loads((root / "Cargo.toml").read_text())
assert manifest.get("lints", {}).get("workspace") is True, \
    "lspf must inherit workspace lints"
assert workspace.get("workspace", {}).get("lints", {}).get("rust", {}).get("unsafe_code") == "forbid", \
    'workspace must set [workspace.lints.rust] unsafe_code = "forbid"'
source = (root / "crates/lspf/src/lib.rs").read_text()
assert re.search(r"^#!\[forbid\(unsafe_code\)\]$", source, re.MULTILINE), \
    "lspf lib.rs must explicitly forbid unsafe_code"
print("lspf safe Rust policy verified")
PY
