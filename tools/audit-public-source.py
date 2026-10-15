"""Scan publishable source/history; print locations and categories, never secret values."""
import argparse
from pathlib import Path
import re
import subprocess

FORBIDDEN = re.compile(r'(^|/)(?:\.work|target|dist|shots|docs/agent)(?:/|$)|'
                       r'(^|/)(?:AGENTS\.md|CLAUDE\.md|roo_task[^/]*|\.env(?:\..*)?|'
                       r'[^/]*-adminsdk-[^/]*\.json|profile(?:-card)?\.(?:png|jpg|enc)|developer-photo\.png)$|'
                       r'\.(?:exe|deb|sqlite3|db|pem|key|ppm|log)$', re.I)
PATTERNS = {
    'private key': re.compile(rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----'),
    'GitHub credential': re.compile(rb'(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{50,})'),
    'Google credential': re.compile(rb'AIza[A-Za-z0-9_-]{35}'),
    'cloud credential': re.compile(rb'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b'),
    'private user path': re.compile(rb'(?i)[A-Z]:[/\\]Users[/\\](?!Public\b|Default\b)[^\s/\\]+'),
    'service account': re.compile(rb'"type"\s*:\s*"service_account"'),
    'private IP': re.compile(rb'\b(?:192\.168\.\d{1,3}\.\d{1,3}|10\.\d{1,3}\.\d{1,3}\.\d{1,3})\b'),
}


def git(*args):
    return subprocess.check_output(['git', *args])


def findings(name, content):
    result = ['private/generated file'] if FORBIDDEN.search(name) else []
    # Binary product images are inspected visually; scanning their compressed
    # byte stream as text produces meaningless matches.
    if b'\0' not in content[:1024]:
        result.extend(label for label, pattern in PATTERNS.items() if pattern.search(content))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--history', action='store_true')
    parser.add_argument('--include-new', action='store_true')
    args = parser.parse_args()
    hits = []
    checked = 0
    if args.history:
        entries = git('rev-list', '--objects', '--all').decode().splitlines()
        for entry in entries:
            sha, _, name = entry.partition(' ')
            if not name or git('cat-file', '-t', sha).strip() != b'blob':
                continue
            checked += 1
            labels = findings(name, git('cat-file', 'blob', sha))
            if labels:
                hits.append(f'{name} [{sha[:10]}]: {", ".join(labels)}')
    else:
        command = ['ls-files', '-z', '--cached']
        if args.include_new:
            command.extend(['--others', '--exclude-standard'])
        names = sorted(set(git(*command).decode().split('\0')) - {''})
        for name in names:
            path = Path(name)
            if not path.is_file():
                continue
            checked += 1
            labels = findings(name, path.read_bytes())
            if labels:
                hits.append(f'{name}: {", ".join(labels)}')
    print(f'Checked {checked} {"history blobs" if args.history else "source files"}.')
    if hits:
        print('\n'.join(hits))
        raise SystemExit(1)
    print('PASS: no private/generated files, personal paths or recognized credential signatures.')


if __name__ == '__main__':
    main()
