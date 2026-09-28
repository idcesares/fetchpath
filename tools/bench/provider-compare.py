"""Fetchpath against the official Hugging Face client (FP-022).

For each case, both download the same repository at the same commit into
fresh folders, with every cache emptied between runs, and the files are
compared by SHA-256. Prints one JSON document. Needs `huggingface_hub` (with
`hf_xet`) in the Python running it, and a built `fetchpath.exe`:

    python tools/bench/provider-compare.py --fetchpath target/release/fetchpath.exe
"""

import argparse
import hashlib
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CASES = [
    # A small repository: ten files, three of them large and Xet-backed.
    {"repo": "hf-internal-testing/tiny-random-gpt2", "file": None},
    # One large Xet-backed file.
    {"repo": "google-bert/bert-base-uncased", "file": "model.safetensors"},
]


def digests(root):
    out = {}
    for path in sorted(Path(root).rglob("*")):
        if path.is_file() and ".cache" not in path.parts:
            out[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return out


def official(repo, file, commit, target, home):
    from huggingface_hub import hf_hub_download, snapshot_download

    os.environ["HF_HOME"] = str(home)
    started = time.perf_counter()
    if file:
        hf_hub_download(repo, file, revision=commit, local_dir=target)
    else:
        snapshot_download(repo, revision=commit, local_dir=target)
    return time.perf_counter() - started


def fetchpath(binary, repo, file, commit, target, data):
    link = f"hf://{repo}@{commit}" + (f"/{file}" if file else "")
    env = dict(os.environ, FETCHPATH_APP_DATA_DIR=str(data))
    started = time.perf_counter()
    subprocess.run([binary, "add", link, "--to", str(target), "--wait", "--quiet"], env=env, check=True,
                   stdout=subprocess.DEVNULL)
    elapsed = time.perf_counter() - started
    subprocess.run([binary, "engine", "stop"], env=env, stdout=subprocess.DEVNULL)
    # The engine leaves shortly after it is asked; its lock goes with it.
    lock = Path(data) / "instance.lock"
    deadline = time.monotonic() + 30
    while lock.exists() and time.monotonic() < deadline:
        try:
            lock.unlink()
        except PermissionError:
            time.sleep(0.2)
    return elapsed, target / repo.split("/")[-1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fetchpath", required=True)
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    import huggingface_hub
    from huggingface_hub import HfApi

    report = {"huggingface_hub": huggingface_hub.__version__, "runs": args.runs, "cases": []}
    for case in CASES:
        info = HfApi().model_info(case["repo"], files_metadata=True)
        commit = info.sha
        result = {"repo": case["repo"], "file": case["file"], "commit": commit,
                  "official_seconds": [], "fetchpath_seconds": [], "identical": True}
        for _ in range(args.runs):
            with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as scratch:
                scratch = Path(scratch)
                seconds = official(case["repo"], case["file"], commit, scratch / "official", scratch / "hf-home")
                result["official_seconds"].append(round(seconds, 2))
                seconds, ours = fetchpath(args.fetchpath, case["repo"], case["file"], commit,
                                          scratch / "fetchpath", scratch / "fetchpath-data")
                result["fetchpath_seconds"].append(round(seconds, 2))
                theirs, mine = digests(scratch / "official"), digests(ours)
                result["files"] = len(mine)
                if theirs != mine:
                    result["identical"] = False
                    result["difference"] = sorted(set(theirs.items()) ^ set(mine.items()))[:10]
        result["official_median"] = statistics.median(result["official_seconds"])
        result["fetchpath_median"] = statistics.median(result["fetchpath_seconds"])
        report["cases"].append(result)
    json.dump(report, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
