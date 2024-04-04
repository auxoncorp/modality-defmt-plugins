#!/usr/bin/env bash

set -euo pipefail

renode-test --kill-stale-renode-instances tests.robot

exit 0
