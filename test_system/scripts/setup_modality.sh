#!/usr/bin/env bash

set -euo pipefail

modality user create --use admin

modality workspace create --use ci-tests config/workspace.toml

modality segment use --latest

conform spec create --file specs/tests.speqtr tests

exit 0
