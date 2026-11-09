#!/usr/bin/env python3
"""Fetch and verify the pinned native Signal runtime for local development."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / 'tools/signal-runtime.json'


def sha(path):
    with path.open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


def ensure_runtime():
    spec = json.loads(MANIFEST.read_text())
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        raise ValueError('The pinned Signal runtime supports Linux x86_64 only')
    cache = ROOT / 'AGENTS/runtime/runtime-cache/signal-cli' / spec['version']
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / '.fetch.lock').open('a+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for kind in ['archive', 'source']:
            asset = spec[kind]
            path = cache / asset['filename']
            if path.is_file() and sha(path) == asset['sha256']:
                continue
            temporary = cache / (asset['filename'] + '.download')
            with urllib.request.urlopen(asset['url'], timeout=45) as response, temporary.open('wb') as output:
                total = 0
                while block := response.read(1024 * 1024):
                    total += len(block)
                    if total > asset['max_bytes']:
                        raise ValueError('Upstream Signal artifact exceeds the pinned byte limit')
                    output.write(block)
                output.flush()
                os.fsync(output.fileno())
            if sha(temporary) != asset['sha256']:
                raise ValueError('Upstream Signal artifact hash does not match the pin')
            temporary.replace(path)
        binary = cache / 'signal-cli'
        valid_binary = binary.is_file() and sha(binary) == spec['binary_sha256']
        if not valid_binary:
            with tarfile.open(cache / spec['archive']['filename']) as archive:
                entry = archive.getmember(spec['member'])
                if not entry.isfile() or entry.size != spec['member_bytes']:
                    raise ValueError('Unexpected native Signal archive layout')
                temporary = cache / '.signal-cli.staging'
                with archive.extractfile(entry) as source, temporary.open('wb') as output:
                    shutil.copyfileobj(source, output, 1024 * 1024)
                    output.flush()
                    os.fsync(output.fileno())
                with temporary.open('rb') as executable:
                    header = executable.read(20)
                if header[:4] != b'\x7fELF' or header[4:6] != b'\x02\x01' or header[18:20] != b'\x3e\x00':
                    raise ValueError('The Signal runtime is not a Linux x86_64 ELF')
                if sha(temporary) != spec['binary_sha256']:
                    raise ValueError('Native Signal executable does not match its pin')
                temporary.chmod(0o755)
                temporary.replace(binary)
        with tarfile.open(cache / spec['source']['filename']) as archive:
            candidates = [entry for entry in archive.getmembers() if entry.isfile() and Path(entry.name).name in ['LICENSE', 'COPYING']]
            if not candidates:
                raise ValueError('Pinned Signal source has no license text')
            for entry in candidates:
                (cache / Path(entry.name).name).write_bytes(archive.extractfile(entry).read())
        return binary, spec


if __name__ == '__main__':
    binary, spec = ensure_runtime()
    print(json.dumps({'version': spec['version'], 'binary': str(binary), 'binary_sha256': sha(binary), 'archive_sha256': spec['archive']['sha256'], 'source_sha256': spec['source']['sha256']}))
