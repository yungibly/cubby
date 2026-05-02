#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/src"
go build -trimpath -o ../keep .
cd ..
makepkg -si
