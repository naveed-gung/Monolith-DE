"""Verify release identity and describe actual packages without downloading anything."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def version():
    player = tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    setup = tomllib.loads((ROOT / 'installer/Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    if not re.fullmatch(r'\d+\.\d+\.\d+', player) or player != setup:
        raise ValueError('Player and setup need matching stable semantic versions.')
    if os.environ.get('GITHUB_REF', '').startswith('refs/tags/'):
        if os.environ.get('GITHUB_REF_NAME') != f'v{player}':
            raise ValueError('Release tag must match Cargo.toml; existing releases are never overwritten.')
    return player


def describe(directory, platform, app_version):
    name = ('monolith-setup.exe' if platform == 'windows-x86_64'
            else f'monolith-desktop_{app_version}_amd64.deb')
    path = directory / name
    magic = b'MZ' if platform == 'windows-x86_64' else b'!<arch>\n'
    with path.open('rb') as stream:
        if stream.read(len(magic)) != magic:
            raise ValueError(f'{name} is not the expected native package.')
        stream.seek(0)
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    if path.stat().st_size <= len(magic):
        raise ValueError('Package is empty.')
    if path.stat().st_size > 100_000_000:
        raise ValueError('Package exceeds the 100 MB updater download limit.')
    return {'name': name, 'platform': platform, 'size': path.stat().st_size, 'sha256': digest}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check-version', action='store_true')
    parser.add_argument('--platform', choices=['windows-x86_64', 'linux-amd64'])
    parser.add_argument('--directory', type=Path, default=ROOT / 'dist')
    args = parser.parse_args()
    app_version = version()
    if args.check_version:
        print(f'Package version: {app_version}')
        return
    if not args.platform:
        parser.error('--platform is required to describe a package')
    package = describe(args.directory, args.platform, app_version)
    (args.directory / f'SHA256SUMS-{args.platform}').write_text(
        f"{package['sha256']}  {package['name']}\n", encoding='utf-8')
    (args.directory / f'package-{args.platform}.json').write_text(json.dumps({
        'schema': 1, 'version': app_version, 'sourceRevision': os.environ.get('GITHUB_SHA'),
        'package': package,
    }, indent=2) + '\n', encoding='utf-8')
    print(f"{package['name']}: {package['size']} bytes; sha256:{package['sha256']}")


if __name__ == '__main__':
    main()
