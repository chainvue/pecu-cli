#!/usr/bin/env python3
"""Classify a set of changed files into one blast-radius level.

Reads `.github/claude-risk-paths.yml` and the paths a pull request touches, and
prints the highest level any of them matches. The automated PR review labels the
PR with the answer and refuses to recommend a merge on `high`.

    git diff --name-only origin/main...HEAD | scripts/classify-risk.py
    scripts/classify-risk.py src/keystore.rs docs/status.md

Output is JSON on stdout::

    {"level": "high", "matched": {"src/keystore.rs": "high", ...}, "unmatched": []}

Exit status is 0 whatever the level is -- the level is data, not a verdict. A
malformed config or an unreadable file is exit 2, because a risk classifier that
fails open is worse than one that fails at all.
"""

import fnmatch
import json
import os
import sys

CONFIG = ".github/claude-risk-paths.yml"

# Ordered from most to least severe. `default` is not a level; it names the one
# a file falls back to when nothing matches.
LEVELS = ("high", "medium", "low")


def load_config(path):
    try:
        import yaml
    except ImportError:  # pragma: no cover - the workflow installs it
        sys.exit("classify-risk: PyYAML is not installed (pip install pyyaml)")

    try:
        with open(path, encoding="utf-8") as handle:
            config = yaml.safe_load(handle)
    except OSError as error:
        sys.exit(f"classify-risk: cannot read {path}: {error}")
    except yaml.YAMLError as error:
        sys.exit(f"classify-risk: {path} is not valid YAML: {error}")

    if not isinstance(config, dict):
        sys.exit(f"classify-risk: {path} must be a mapping")

    fallback = config.get("default", "medium")
    if fallback not in LEVELS:
        sys.exit(f"classify-risk: default must be one of {LEVELS}, got {fallback!r}")

    globs = {}
    for level in LEVELS:
        patterns = config.get(level, []) or []
        if not isinstance(patterns, list) or any(not isinstance(p, str) for p in patterns):
            sys.exit(f"classify-risk: {level} must be a list of glob strings")
        globs[level] = patterns

    unknown = set(config) - set(LEVELS) - {"default"}
    if unknown:
        sys.exit(f"classify-risk: unknown keys in {path}: {sorted(unknown)}")

    return fallback, globs


def matches(path, pattern):
    """fnmatch, with `**` meaning "this directory and everything under it".

    fnmatch on its own treats `*` as matching across separators, which would
    make `src/*.rs` match `src/cmd/send.rs` and quietly widen every rule. So a
    pattern without `**` is matched segment-wise, and `dir/**` is turned into a
    prefix test -- which is what a person writing this file expects both to do.
    """
    if pattern.endswith("/**"):
        prefix = pattern[:-3]
        return path == prefix or path.startswith(prefix + "/")
    if "**" in pattern:
        # `**/*.snap` and friends: anchor the tail, ignore the depth.
        head, _, tail = pattern.partition("**")
        if head and not path.startswith(head):
            return False
        tail = tail.lstrip("/")
        if not tail:
            return True
        return any(
            fnmatch.fnmatch("/".join(path.split("/")[i:]), tail)
            for i in range(len(path.split("/")))
        )
    if "/" not in pattern:
        return fnmatch.fnmatch(os.path.basename(path), pattern)
    parts, globs = path.split("/"), pattern.split("/")
    if len(parts) != len(globs):
        return False
    return all(fnmatch.fnmatch(part, glob) for part, glob in zip(parts, globs))


def classify(paths, fallback, globs):
    matched, unmatched = {}, []
    for path in paths:
        for level in LEVELS:
            if any(matches(path, pattern) for pattern in globs[level]):
                matched[path] = level
                break
        else:
            matched[path] = fallback
            unmatched.append(path)

    highest = "low"
    for level in LEVELS:
        if level in matched.values():
            highest = level
            break
    # An empty pull request is not low risk, it is nothing. Say so rather than
    # letting "no files" read as "safe".
    if not paths:
        highest = "low"
    return {"level": highest, "matched": matched, "unmatched": sorted(unmatched)}


def main():
    paths = sys.argv[1:]
    if not paths:
        paths = [line.strip() for line in sys.stdin if line.strip()]
    paths = [p for p in paths if p]

    config = os.environ.get("CLAUDE_RISK_PATHS", CONFIG)
    fallback, globs = load_config(config)
    json.dump(classify(paths, fallback, globs), sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
