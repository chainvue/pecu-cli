#!/usr/bin/env python3
"""Validate the `--json-schema` arguments embedded in the Claude workflows.

    scripts/check-workflow-schemas.py

Three of the first four failures of this pipeline were the same shape: a config
value that is correct as far as the file is concerned and wrong to whatever
consumes it, discovered only by burning a workflow run. A schema is the worst
case of that -- it is a JSON document inside a single-quoted shell word inside a
YAML block scalar, so it has three ways to be broken that YAML validation does
not see, and the failure surfaces minutes into a run that has already spent
tokens.

Checked, per schema:

  1. It parses as JSON at all.
  2. It contains no apostrophe. The schema is passed to the CLI inside a
     single-quoted shell word, so one apostrophe in a description truncates the
     document and the run dies with "Unterminated string". Use backticks.
  3. It contains no newline, for the same reason one level up: claude_args is a
     YAML block scalar split into one flag per line.
  4. Every property has a description. Without one the model is told what shape
     an answer takes and nothing about what would make it true, which is how a
     verdict comes back reading `{"begruendung": "test"}`.

Exit status is 1 on any finding, so this belongs in front of a push.
"""

import glob
import json
import re
import sys

PATTERN = re.compile(r"--json-schema '(.*?)'\n")


def properties(node, path="$"):
    """Yield (json-path, subschema) for every declared property, recursively."""
    if not isinstance(node, dict):
        return
    for name, sub in (node.get("properties") or {}).items():
        here = f"{path}.{name}"
        yield here, sub
        yield from properties(sub, here)
        if isinstance(sub.get("items"), dict):
            yield from properties(sub["items"], f"{here}[]")


def check(path, raw, index):
    label = f"{path} schema #{index}"
    findings = []

    if "'" in raw:
        findings.append("contains an apostrophe; it would truncate the shell word (use a backtick)")
    if "\n" in raw:
        findings.append("contains a newline; claude_args is one flag per line")

    try:
        schema = json.loads(raw)
    except json.JSONDecodeError as error:
        findings.append(f"is not valid JSON: {error}")
        return [f"{label} {f}" for f in findings]

    missing = [name for name, sub in properties(schema) if "description" not in sub]
    if missing:
        findings.append(f"has {len(missing)} property/properties with no description: {missing}")

    required = set(schema.get("required") or [])
    declared = set((schema.get("properties") or {}).keys())
    if required != declared:
        findings.append(
            f"top-level required {sorted(required)} does not match properties {sorted(declared)}")

    return [f"{label} {f}" for f in findings]


def main():
    files = sorted(glob.glob(".github/workflows/claude-*.yml"))
    if not files:
        sys.exit("no .github/workflows/claude-*.yml found; run from the repository root")

    findings, count = [], 0
    for path in files:
        for index, raw in enumerate(PATTERN.findall(open(path, encoding="utf-8").read()), 1):
            count += 1
            findings += check(path, raw, index)

    if not count:
        sys.exit("no --json-schema arguments found; has the flag been renamed?")

    for finding in findings:
        print(f"  {finding}")
    print(f"{count} schema(s) checked, {len(findings)} finding(s)")
    sys.exit(1 if findings else 0)


if __name__ == "__main__":
    main()
