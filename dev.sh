#!/usr/bin/env bash
set -e

podman build -t cubby-dev .

podman run --rm -it \
  -v .:/app:Z \
  -w /app/src \
  cubby-dev
