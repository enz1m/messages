#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - "$@" <<'PY'
import argparse,importlib.util,json,subprocess,shutil
from pathlib import Path
parser=argparse.ArgumentParser(description='Install the pinned native Signal runtime beside a development build.')
parser.add_argument('--release',action='store_true',help='Install beside the release build instead of debug.')
args=parser.parse_args()
spec=importlib.util.spec_from_file_location('signal_runtime','tools/fetch-signal.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
binary,pin=module.ensure_runtime()
metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps','--format-version','1'],text=True))
folder=Path(metadata['target_directory'])/('release' if args.release else 'debug');folder.mkdir(parents=True,exist_ok=True)
shutil.copy2(binary,folder/'signal-cli')
print('Installed pinned Signal runtime to '+str(folder/'signal-cli'))
PY
