#!/usr/bin/env bash

set -euo pipefail

if [ ! -d modality-robot-framework ]; then
    git clone --depth 1 https://github.com/auxoncorp/modality-robot-framework.git
fi

python3 -m venv .env
source .env/bin/activate
pip install maturin
pip install patchelf
pip install -r /opt/renode/tests/requirements.txt
pip install appdirs ipykernel pandas plotly plotly-express nbformat

(
    cd modality-robot-framework
    maturin develop --release --target x86_64-unknown-linux-gnu
)

exit 0
