#!/usr/bin/env bash
# Enforce the crate boundary that PLAN.md 2 calls boundaries 1 and 3.
#
#   Only `palisade-git` and `palisade-exec` may spawn a process.
#
# Why a script and not a lint: the invariant is *about which crate is allowed
# to do something*, and a grep across the workspace is the cheapest honest way
# to say that. It matches text rather than types, so it is paired with the
# compile-time guarantee that `Origin::Delegated` is constructible only in
# `palisade-exec` and that `Origin` has no `Judged` variant at all.
#
# A gate whose output depends on something outside the observation is not
# calibratable, and a gate with no meaningful false-positive rate has no
# business blocking anybody's work. This script stops that happening by
# accident, and stops a future contributor adding `Command::new` to a gate.

set -euo pipefail

cd "$(dirname "$0")/.."

# `palisade-git`    — the repository, as a read-only data source.
# `palisade-exec`   — the delegated tools, and the exit-code table.
# `palisade-testkit`— dev-dependency only; building a fixture repo is its job.
ALLOWED=(palisade-git palisade-exec palisade-testkit)

# Spawning specifically. `std::process::ExitCode` and `std::process::id` are
# not spawning, and a check that flags them is a check people disable.
SPAWNS='process::Command|Command::new|Stdio::|\.spawn\(|\.output\(|process::abort'

status=0

for manifest in crates/*/Cargo.toml; do
  crate="$(basename "$(dirname "$manifest")")"

  [[ " ${ALLOWED[*]} " == *" $crate "* ]] && continue

  # Comments are documentation, and this crate's own comment says it must not
  # spawn. A check that trips on the sentence describing the rule gets the
  # rule deleted.
  hits="$(
    grep -rnE "$SPAWNS" --include='*.rs' "crates/$crate/src" 2>/dev/null \
      | grep -vE '^[^:]+:[0-9]+: *(//|///|//!)' \
      || true
  )"

  if [[ -n "$hits" ]]; then
    echo "boundary: $crate spawns a process; only ${ALLOWED[*]} may do so"
    echo "$hits" | sed 's/^/    /'
    status=1
  fi
done

if [[ $status -eq 0 ]]; then
  echo "boundary: ok — process execution confined to ${ALLOWED[*]}"
fi

exit $status
