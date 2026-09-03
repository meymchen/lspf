#!/usr/bin/env bash

# Hold the 1.0 interface freeze: docs/public-interface.md and the crate must
# agree in both directions. A new export that nobody inventoried, an inventory
# row whose export is gone, an export that moved to another feature or target,
# a catalog descriptor no journey registers, and a Cargo feature the support
# contract never describes all fail here.

set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

readonly INVENTORY=docs/public-interface.md
readonly LIB=crates/lspf/src/lib.rs
readonly TESTING=crates/lspf/src/testing.rs
readonly FEATURES=crates/lspf/src/features.rs
readonly CATALOG=crates/lspf/tests/catalog.rs
readonly MANIFEST=crates/lspf/Cargo.toml
readonly SUPPORT=SECURITY.md

status=0
fail() {
    printf 'public-interface error: %s\n' "$*" >&2
    status=1
}

for required in "$INVENTORY" "$LIB" "$TESTING" "$FEATURES" "$CATALOG" \
    "$MANIFEST" "$SUPPORT"
do
    if [[ ! -f "$root/$required" ]]; then
        printf 'public-interface error: missing required input: %s\n' \
            "$root/$required" >&2
        exit 1
    fi
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- The inventory ---------------------------------------------------------
#
# Each table is recognized by its header row, so the document stays readable
# prose rather than a generated data file.

awk -v out="$work" '
function cell(line, index_,   n, parts, value) {
    n = split(line, parts, "|")
    value = parts[index_ + 1]
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    gsub(/`/, "", value)
    return value
}
/^\| Availability \| Cargo cfg \| Meaning \|$/ { mode = "availability"; next }
/^\| Item \| Availability \| Role \|$/ { mode = "frozen"; next }
/^\| Item \| Availability \| Why it is not frozen \|$/ { mode = "unfrozen"; next }
/^\| Item \| Role \|$/ { mode = "testing"; next }
/^\| Alias \| Generated type \| Why the alias is frozen \|$/ { mode = "alias"; next }
/^\|/ {
    if (mode == "") next
    if ($0 ~ /^\|[[:space:]]*-+/) next
    if (mode == "availability") {
        gate = cell($0, 2)
        if (gate == "*(ungated)*") gate = "-"
        printf "%s\t%s\n", gate, cell($0, 1) >>(out "/availability")
    } else if (mode == "frozen") {
        printf "%s\t%s\n", cell($0, 1), cell($0, 2) >>(out "/frozen")
    } else if (mode == "unfrozen") {
        printf "%s\t%s\n", cell($0, 1), cell($0, 2) >>(out "/unfrozen")
    } else if (mode == "testing") {
        printf "%s\n", cell($0, 1) >>(out "/testing-frozen")
    } else if (mode == "alias") {
        printf "%s\n", cell($0, 1) >>(out "/alias-frozen")
    }
    next
}
{ mode = "" }
' "$root/$INVENTORY"

for produced in availability frozen unfrozen testing-frozen alias-frozen; do
    [[ -f "$work/$produced" ]] || : >"$work/$produced"
done

# --- The crate root --------------------------------------------------------
#
# lib.rs is the only file that declares the crate root's exports. Items are
# recognized at column zero; anything indented belongs to a nested module and
# is inventoried by its own section below.

awk '
BEGIN { cfg = "-"; hidden = 0; buf = ""; collecting = 0 }
/^#\[cfg\(/ { cfg = $0; next }
/^#\[doc\(hidden\)\]/ { hidden = 1; next }
collecting == 1 {
    buf = buf " " $0
    if ($0 ~ /;[[:space:]]*$/) {
        emit(buf, cfg, hidden); cfg = "-"; hidden = 0; buf = ""; collecting = 0
    }
    next
}
/^pub use / {
    buf = $0
    if ($0 ~ /;[[:space:]]*$/) { emit(buf, cfg, hidden); cfg = "-"; hidden = 0; buf = "" }
    else collecting = 1
    next
}
/^pub / {
    name = declaration_name($0)
    if (name == "") name = "<unrecognized-public-declaration>"
    printf "%s\t%s\t%s\n", name, cfg, hidden
    cfg = "-"; hidden = 0
    next
}
/^[^ \t\/#]/ { cfg = "-"; hidden = 0; next }
function declaration_name(text,   n, i, parts, token, name) {
    n = split(text, parts, /[[:space:]]+/)
    for (i = 2; i <= n; i++) {
        token = parts[i]
        if (token == "const" && parts[i + 1] == "fn") continue
        if (token == "extern" && parts[i + 1] == "crate") {
            name = parts[i + 2]
            break
        }
        if (token == "static" && parts[i + 1] == "mut") {
            name = parts[i + 2]
            break
        }
        if (token ~ /^(mod|const|static|struct|enum|union|trait|type|fn|macro)$/) {
            name = parts[i + 1]
            break
        }
    }
    gsub(/[(<{;:=].*/, "", name)
    return name
}
function emit(text, cfg_, hidden_,   inner, n, i, parts, name) {
    sub(/^pub use /, "", text)
    sub(/;[[:space:]]*$/, "", text)
    inner = text
    if (text ~ /\{/) { sub(/^[^{]*\{/, "", inner); sub(/\}[^}]*$/, "", inner) }
    n = split(inner, parts, ",")
    for (i = 1; i <= n; i++) {
        name = parts[i]
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
        if (name == "") continue
        if (name ~ / as /) sub(/^.* as /, "", name)
        else sub(/^.*::/, "", name)
        printf "%s\t%s\t%s\n", name, cfg_, hidden_
    }
}
' "$root/$LIB" >"$work/root-exports"

# Load the inventory into memory once. Spawning a matcher per export makes a
# gate that runs on every pull request needlessly slow.
declare -A availability_key frozen_availability unfrozen_availability root_export

while IFS=$'\t' read -r gate key; do
    if [[ -n $gate ]]; then availability_key["$gate"]=$key; fi
done <"$work/availability"
while IFS=$'\t' read -r name key; do
    if [[ -n $name ]]; then frozen_availability["$name"]=$key; fi
done <"$work/frozen"
while IFS=$'\t' read -r name key; do
    if [[ -n $name ]]; then unfrozen_availability["$name"]=$key; fi
done <"$work/unfrozen"

while IFS=$'\t' read -r name gate hidden; do
    [[ -n $name ]] || continue
    root_export["$name"]=1
    key=${availability_key["$gate"]-}
    if [[ -z $key ]]; then
        fail "crate root export '$name' uses an availability gate $INVENTORY does not define: ${gate/#-/(ungated)}"
        continue
    fi
    frozen_key=${frozen_availability["$name"]-}
    unfrozen_key=${unfrozen_availability["$name"]-}
    if [[ $hidden == 1 ]]; then
        if [[ -n $frozen_key ]]; then
            fail "crate root export '$name' is hidden from documentation, so it cannot be frozen"
        fi
        if [[ -z $unfrozen_key ]]; then
            fail "crate root export '$name' is hidden from documentation and $INVENTORY does not record why it is not frozen"
        elif [[ $unfrozen_key != "$key" ]]; then
            fail "crate root export '$name' is available under '$key', recorded as '$unfrozen_key'"
        fi
        continue
    fi
    if [[ -n $unfrozen_key ]]; then
        fail "crate root export '$name' is documented, so $INVENTORY must freeze it rather than exclude it"
        continue
    fi
    if [[ -z $frozen_key ]]; then
        fail "crate root exports '$name', which $INVENTORY does not freeze"
        continue
    fi
    if [[ $frozen_key != "$key" ]]; then
        fail "crate root export '$name' is available under '$key', frozen as '$frozen_key'"
    fi
done <"$work/root-exports"

for name in "${!frozen_availability[@]}"; do
    [[ -n ${root_export["$name"]-} ]] \
        || fail "$INVENTORY freezes '$name', which the crate root does not export"
done

# --- lspf::testing ---------------------------------------------------------

awk '
collecting == 1 {
    buf = buf " " $0
    if ($0 ~ /;[[:space:]]*$/) {
        emit(buf); buf = ""; collecting = 0
    }
    next
}
/^pub use / {
    buf = $0
    if ($0 ~ /;[[:space:]]*$/) { emit(buf); buf = "" }
    else collecting = 1
    next
}
/^pub / {
    name = declaration_name($0)
    if (name == "") name = "<unrecognized-public-declaration>"
    print name
}
function declaration_name(text,   n, i, parts, token, name) {
    n = split(text, parts, /[[:space:]]+/)
    for (i = 2; i <= n; i++) {
        token = parts[i]
        if (token == "const" && parts[i + 1] == "fn") continue
        if (token == "extern" && parts[i + 1] == "crate") {
            name = parts[i + 2]
            break
        }
        if (token == "static" && parts[i + 1] == "mut") {
            name = parts[i + 2]
            break
        }
        if (token ~ /^(mod|const|static|struct|enum|union|trait|type|fn|macro)$/) {
            name = parts[i + 1]
            break
        }
    }
    gsub(/[(<{;:=].*/, "", name)
    return name
}
function emit(text,   inner, n, i, parts, name) {
    sub(/^pub use /, "", text)
    sub(/;[[:space:]]*$/, "", text)
    inner = text
    if (text ~ /\{/) { sub(/^[^{]*\{/, "", inner); sub(/\}[^}]*$/, "", inner) }
    n = split(inner, parts, ",")
    for (i = 1; i <= n; i++) {
        name = parts[i]
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
        if (name == "") continue
        if (name ~ / as /) sub(/^.* as /, "", name)
        else sub(/^.*::/, "", name)
        print name
    }
}
' "$root/$TESTING" | LC_ALL=C sort -u >"$work/testing-actual"
LC_ALL=C sort -u "$work/testing-frozen" >"$work/testing-frozen-sorted"

while read -r name; do
    if [[ -n $name ]]; then
        fail "lspf::testing exports '$name', which $INVENTORY does not freeze"
    fi
done < <(comm -23 "$work/testing-actual" "$work/testing-frozen-sorted")
while read -r name; do
    if [[ -n $name ]]; then
        fail "$INVENTORY freezes 'lspf::testing::$name', which the module does not export"
    fi
done < <(comm -13 "$work/testing-actual" "$work/testing-frozen-sorted")

# --- lspf::types aliases ---------------------------------------------------
#
# The four-space indentation selects the alias block in `types` itself; the
# marker re-exports inside `types::request` and `types::notification` are
# nested one level deeper and are pinned by the catalog fixture instead.

while IFS= read -r declaration; do
    [[ -n $declaration ]] \
        && fail "lspf::types contains unsupported public declaration: $declaration"
done < <(awk '
/^pub mod types \{$/ { inside = 1; next }
inside && /^}$/ { inside = 0; next }
!inside { next }
/^    pub use gen_lsp_types::\*;$/ { next }
/^    pub use gen_lsp_types::\{$/ { next }
/^    pub mod (request|notification) \{$/ { next }
/^        pub trait (Request|Notification) \{$/ { next }
/^        pub use gen_lsp_types::\{$/ { next }
/^    pub / || /^        pub / { print }
' "$root/$LIB")

awk '
/^    pub use gen_lsp_types::\{$/ { inside = 1; next }
inside && /^    \};$/ { inside = 0; next }
inside && / as / {
    line = $0
    gsub(/^[[:space:]]+|[[:space:]]*,?[[:space:]]*$/, "", line)
    n = split(line, parts, ",")
    for (i = 1; i <= n; i++) {
        entry = parts[i]
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", entry)
        if (entry !~ / as /) continue
        sub(/^.* as /, "", entry)
        print entry
    }
}' "$root/$LIB" | LC_ALL=C sort -u >"$work/alias-actual"

LC_ALL=C sort -u "$work/alias-frozen" >"$work/alias-frozen-sorted"
while read -r name; do
    if [[ -n $name ]]; then
        fail "lspf::types aliases '$name', which $INVENTORY does not freeze"
    fi
done < <(comm -23 "$work/alias-actual" "$work/alias-frozen-sorted")
while read -r name; do
    if [[ -n $name ]]; then
        fail "$INVENTORY freezes the alias 'lspf::types::$name', which the crate does not define"
    fi
done < <(comm -13 "$work/alias-actual" "$work/alias-frozen-sorted")

# --- lspf::features --------------------------------------------------------
#
# The catalog journey is the fixture that pins every advertised capability, so
# a descriptor it never registers would enter the interface unpinned. Public
# descriptor types are paired with those constructors; any other public type
# would be an accidental implementation export.

while IFS= read -r declaration; do
    [[ -n $declaration ]] \
        && fail "lspf::features contains unsupported public declaration: $declaration"
done < <(awk '/^pub / && $0 !~ /^pub (fn|struct|trait) /' "$root/$FEATURES")

awk '
/^pub fn / {
    signature = $0
    collecting = ($0 !~ /\{/)
    if (!collecting) emit(signature)
    next
}
collecting {
    signature = signature " " $0
    if ($0 ~ /\{/) {
        emit(signature)
        collecting = 0
    }
}
function emit(text,   name, result) {
    name = text
    sub(/^pub fn /, "", name)
    sub(/\(.*/, "", name)
    result = text
    sub(/^.*->[[:space:]]*/, "", result)
    sub(/[[:space:]<{].*$/, "", result)
    printf "%s\t%s\n", name, result
}
' "$root/$FEATURES" | LC_ALL=C sort -u >"$work/descriptor-signatures"
cut -f1 "$work/descriptor-signatures" >"$work/descriptors"
cut -f2 "$work/descriptor-signatures" | LC_ALL=C sort -u \
    >"$work/descriptor-result-types"

awk '/^pub struct / {
    name = $3
    gsub(/[<(\{;].*/, "", name)
    print name
}' "$root/$FEATURES" | LC_ALL=C sort -u >"$work/descriptor-types"

declare -A descriptor_result_type descriptor_type
while read -r type; do
    [[ -n $type ]] && descriptor_result_type["$type"]=1
done <"$work/descriptor-result-types"
while read -r type; do
    [[ -n $type ]] && descriptor_type["$type"]=1
done <"$work/descriptor-types"

for type in "${!descriptor_type[@]}"; do
    if [[ -z ${descriptor_result_type["$type"]-} ]]; then
        fail "lspf::features exports type '$type', which no catalog descriptor returns"
    fi
done

for type in "${!descriptor_result_type[@]}"; do
    if [[ -z ${descriptor_type["$type"]-} ]]; then
        fail "feature descriptor returns '$type', which is not a public descriptor type"
    fi
done

while read -r name; do
    [[ -n $name ]] || continue
    if [[ $name != FeatureSpec && $name != NotificationFeatureSpec ]]; then
        fail "lspf::features exports unexpected public trait '$name'"
    fi
done < <(awk '/^pub trait / {
    name = $3
    gsub(/[<(\{;:=].*/, "", name)
    print name
}' "$root/$FEATURES")

grep -o 'features::[a-z0-9_]*(' "$root/$CATALOG" \
    | sed 's/^features:://; s/($//' \
    | LC_ALL=C sort -u >"$work/registered-descriptors"

while read -r descriptor; do
    if [[ -n $descriptor ]]; then
        fail "feature descriptor 'features::$descriptor' is not registered by $CATALOG"
    fi
done < <(comm -23 "$work/descriptors" "$work/registered-descriptors")

# --- Cargo features --------------------------------------------------------
#
# `default` is described in prose as a selection rather than named as a
# feature; every other feature must appear in the support contract's table.

while read -r feature; do
    [[ -n $feature && $feature != default ]] || continue
    grep -Fq "\`$feature\`" "$root/$SUPPORT" \
        || fail "Cargo feature '$feature' is not described by $SUPPORT"
done < <(awk '
    /^\[features\]/ { inside = 1; next }
    /^\[/ { inside = 0 }
    inside && /^[a-zA-Z0-9_-]+[[:space:]]*=/ { name = $1; print name }
' "$root/$MANIFEST")

if ((status != 0)); then
    exit 1
fi

printf 'Public interface freeze verified: %s crate-root exports, %s testing items, %s type aliases\n' \
    "$(wc -l <"$work/frozen" | tr -d ' ')" \
    "$(wc -l <"$work/testing-frozen" | tr -d ' ')" \
    "$(wc -l <"$work/alias-frozen" | tr -d ' ')"
