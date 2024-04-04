#!/usr/bin/env bash

set -euo pipefail

renode-test --jobs 1 --kill-stale-renode-instances --variable CREATE_SNAPSHOT_ON_FAIL:False tests.robot

exit 0
