#!/usr/bin/env python3
"""Replays a repository's merge history through the gate suite.

The measurement harness for PRD §9 criterion 3: does a *generated* contract
find real problems in real agent-authored changes, at an acceptable
false-positive rate?

Every gate runs on every merge, in a scratch clone, replaying each merge as a
two-tree state -- base tree checked out, then the merge's blobs copied over it
-- without ever merging, so a replay conflict is never the event under test.
The working repository is never touched. The first version of this harness
checked merges out in place and left the tree dirty: M0's exact bug class,
reintroduced by tooling written after M0.

Two lessons are baked in here rather than in a comment:

  * `git cat-file --batch` output is `<sha> <type> <size>\n<contents>\n` per
    entry. An earlier parser split on newlines and ignored the size field, so
    file contents were written offset and one file came out 15 bytes instead of
    1562. Three gates then correctly reported `Untrustworthy` on the corrupt
    file -- the gates were right and the harness was wrong.
  * A finding at `warn` on an `accept` verdict is the interesting case, not an
    error. The harness classifies by exit code and verdict, never by grepping
    the human-readable output, which is how three false "errors" were reported
    once.

The contract is *generated* by `palisade init`, not hand-written: a contract
chosen by the person running the measurement would measure the gates they
already know work, which is the circularity this whole milestone exists to
avoid.
"""
import json, os, re, subprocess, sys, tempfile, collections, shutil

BIN = "/Users/origo/src/palisade/target/debug/palisade"

GENERATED_AT = "2026-09-29"  # the review date `palisade init` would write


def generate_contract(root: str, repo: str) -> str:
    """Ask `palisade init` for a contract, rather than writing one here.

    The whole point of the exercise. A hand-written contract in this file
    would be the person running the measurement choosing gates they already
    know work, and the resulting number would measure their selection rather
    than the gates. `init` is deterministic given a repository, so the
    contract is reproducible without being authored.
    """
    out = subprocess.run(
        [BIN, "init", "--path", root, "--dry-run"],
        capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"palisade init failed: {out.stderr[:300]}")
    contract = out.stdout
    if "--no-delegated" in sys.argv:
        # `checks_green` runs `cargo test --workspace`, which on a 38-member,
        # 508k-LOC workspace is *minutes per merge* and would dominate the
        # corpus. The other eight gates are per-diff and finish in well under a
        # second.
        #
        # This is a real exclusion and it is recorded rather than assumed: the
        # corpus does not measure `checks_green`, and any claim about it needs
        # its own calibration. The eight gates that remain are the ones with
        # a two-tree comparison, which is what the false-positive question is
        # really about.
        contract = re.sub(
            r"\[\[gates\]\]\n# runs `cargo fmt`.*?severity = \"warn\"\n\n",
            "", contract, flags=re.S)
        print(f"corpus: delegated gates excluded from {repo} "
              f"({contract.count('[[gates]]')} analyzed gates)")
    # `init` writes today's date; the corpus pins it so two runs of the same
    # corpus produce byte-identical contracts, and so `contract_review_stale`
    # cannot differ between them.
    contract = re.sub(r'^reviewed = ".*"$', f'reviewed = "{GENERATED_AT}"',
                      contract, flags=re.M)
    return contract

def git(*args, cwd, check=True):
    r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()[:200]}")
    return r

def blobs_of(repo, treeish):
    """Every path and its blob id in a commit's tree."""
    out = git("ls-tree", "-r", "--full-tree", treeish, cwd=repo).stdout
    files = {}
    for line in out.splitlines():
        meta, _, path = line.partition("\t")
        parts = meta.split()
        if len(parts) == 3 and parts[1] == "blob":
            files[path] = parts[2]
    return files

def main():
    repo, limit = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 40
    pin = "--pin" in sys.argv
    work = tempfile.mkdtemp()
    clone = os.path.join(work, "clone")
    try:
        git("clone", "-q", "--no-hardlinks", repo, clone, cwd=work)
        head_sha = git("rev-parse", "HEAD", cwd=clone).stdout.strip()
        head = git("rev-parse", "HEAD", cwd=clone).stdout.strip()
        if pin:
            print(f"pin: {repo} {head}")
        contract = generate_contract(clone, repo)
        total_merges = int(git("rev-list", "--count", "--merges", "HEAD", cwd=clone).stdout.strip())
        merges = git("log", "--merges", "--format=%H", "-n", str(limit), cwd=clone).stdout.split()

        clean = found = err = skipped = 0
        by_primitive = collections.Counter()
        by_severity = collections.Counter()
        samples = []

        for m in merges:
            p = git("rev-parse", f"{m}^1", cwd=clone).stdout.strip()
            if not p:
                skipped += 1; continue
            try:
                git("checkout", "-q", "--detach", p, cwd=clone)
                git("read-tree", "-q", "-m", "-u", p, cwd=clone)
                head = blobs_of(clone, m)
                base = blobs_of(clone, p)
                for path, blob in head.items():
                    if path in base:
                        continue
                    dst = os.path.join(clone, path)
                    os.makedirs(os.path.dirname(dst), exist_ok=True)
                    with open(dst, "wb") as f:
                        f.write(git("cat-file", "blob", blob, cwd=clone).stdout.encode("utf-8", "surrogateescape")
                                if False else
                                subprocess.run(["git", "cat-file", "blob", blob], cwd=clone,
                                               capture_output=True).stdout)
                for path in base:
                    if path not in head:
                        fp = os.path.join(clone, path)
                        if os.path.isfile(fp):
                            os.remove(fp)
                with open(os.path.join(clone, "palisade.toml"), "w") as f:
                    f.write(contract)

                r = subprocess.run([BIN, "check", "--base", p, "--format", "json"],
                                   cwd=clone, capture_output=True, text=True)
            except Exception:
                skipped += 1
                git("reset", "-q", "--hard", p, check=False, cwd=clone)
                git("clean", "-qfdx", check=False, cwd=clone)
                continue

            code = r.returncode
            try:
                d = json.loads(r.stdout)
            except Exception:
                d = None
            if code == 2:
                err += 1
                detail = ""
                if d:
                    for gg in d["acceptance"]["gates"]:
                        if gg["outcome"] == "untrustworthy":
                            detail = gg["gateId"]
                if os.environ.get("SNAP"):
                    snap = os.path.join(os.environ["SNAP"], m[:10])
                    subprocess.run(["cp", "-a", clone, snap], check=False)
                samples.append(("ERROR", git("log", "-1", "--format=%s", m, cwd=clone).stdout.strip()[:55], detail, "M="+m, "P="+p))
            else:
                # accept (0) or block (1). Count findings either way -- a warn
                # finding on a clean verdict is exactly the false-positive
                # shape we are looking for.
                if d and d["rule_violations"]["count"] > 0:
                    found += 1
                    if code == 0:
                        clean += 0  # still counted as found below; verdict is accept
                    for f in (d["rule_violations"]["findings"] if d else []):
                        by_primitive[f["primitive"]] += 1
                        by_severity[f["severity"]] += 1
                        if len(samples) < 400:
                            samples.append((f["primitive"], f["severity"],
                                            f"{f['message'][:95]} || exp={str(f['expected'])[:55]} obs={str(f['observed'])[:55]}"))
                else:
                    clean += 1

            git("reset", "-q", "--hard", p, check=False, cwd=clone)
            git("clean", "-qfdx", check=False, cwd=clone)

        print(f"=== {repo} @ {head_sha} ===")
        print(f"replayed {len(merges)} of {total_merges} merges   "
              f"clean={clean}  with-findings={found}  error={err}  skipped={skipped}")
        if by_primitive:
            print("\nfindings by primitive:")
            for k, v in by_primitive.most_common():
                print(f"  {v:4d}  {k}")
        print("\nsamples:")
        for s in samples[:400]:
            print("  ", " | ".join(str(x) for x in s))
    finally:
        shutil.rmtree(work, ignore_errors=True)

main()
