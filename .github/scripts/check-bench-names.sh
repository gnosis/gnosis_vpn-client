#!/usr/bin/env bash
# Validate that all [[bench]] entries in Cargo.toml files have names ending with _bench.
set -euo pipefail
exit_code=0
for file in "$@"; do
    while IFS= read -r line; do
        if [[ $line =~ ^name[[:space:]]*=[[:space:]]*\"([^\"]+)\" ]]; then
            name="${BASH_REMATCH[1]}"
            if [[ ! $name =~ _bench$ ]]; then
                echo "ERROR: benchmark name '$name' in $file must end with _bench" >&2
                exit_code=1
            fi
        fi
    done < <(awk '/^\[\[bench\]\]/{found=1; next} found && /^\[/{found=0} found{print}' "$file")
done
exit "$exit_code"
