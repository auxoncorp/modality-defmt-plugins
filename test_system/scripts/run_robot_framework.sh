#!/usr/bin/env bash

set -euo pipefail

if [ ! -d .env ]; then
    echo "python venv is missing, did you run scripts/setup_robot_framework.sh?"
    exit 1
fi

source .env/bin/activate

renode-test --jobs 1 --kill-stale-renode-instances --variable CREATE_SNAPSHOT_ON_FAIL:False test_suite.robot

exit 0
