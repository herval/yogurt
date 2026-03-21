#!/bin/bash
set -e
cd "$(dirname "$0")"
go build -o yogurtgo . && ./yogurtgo "$@"
