#!/usr/bin/env python3
"""Check every column the seed files write against the target database.

A seed written against a hand-modified database can reference columns that no
migration creates. Postgres reports only the first such column and only once it
is reached, so the failure arrives mid-file with most of the seed already
applied and no indication of how many more are wrong. This lists all of them
before anything is written.

  check-seed-columns.py <psql> <database-url> <file.sql> [file.sql ...]
"""
import re
import subprocess
import sys


def insert_targets(sql: str):
    """(table, [columns]) for every INSERT ... (cols) in the file."""
    for m in re.finditer(r'INSERT\s+INTO\s+([a-z_][a-z0-9_]*)\s*\(([^)]*)\)',
                         sql, re.I):
        cols = [c.strip() for c in m.group(2).split(',') if c.strip()]
        # Only plain column lists; anything with an expression is not a target list.
        if all(re.fullmatch(r'[a-z_][a-z0-9_]*', c) for c in cols):
            yield m.group(1), cols


def main() -> int:
    psql, url, *files = sys.argv[1:]
    out = subprocess.run(
        [psql, url, '-At', '-F', '|', '-c',
         "SELECT table_name, column_name FROM information_schema.columns "
         "WHERE table_schema = 'meridian'"],
        capture_output=True, text=True)
    if out.returncode != 0:
        print(out.stderr.strip(), file=sys.stderr)
        return 1

    actual: dict[str, set[str]] = {}
    for line in out.stdout.splitlines():
        if '|' in line:
            t, c = line.split('|', 1)
            actual.setdefault(t, set()).add(c)

    problems = []
    for path in files:
        sql = open(path, encoding='utf-8').read()
        for table, cols in insert_targets(sql):
            if table not in actual:
                problems.append(f"{path}: no table '{table}'")
                continue
            for c in cols:
                if c not in actual[table]:
                    problems.append(f"{path}: {table}.{c} does not exist")

    if problems:
        print(f"  {len(problems)} column(s) the target database does not have:",
              file=sys.stderr)
        for p in problems:
            print(f"    {p}", file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
