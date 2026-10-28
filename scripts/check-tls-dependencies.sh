#!/usr/bin/env bash
# Read `cargo tree --prefix none --format '{p}'` from stdin.
set -euo pipefail

profile="${1:-}"
case "$profile" in
    production|ring|aws-lc) ;;
    *)
        echo "usage: check-tls-dependencies.sh production|ring|aws-lc" >&2
        exit 2
        ;;
esac

ring=0
aws_rs=0
aws_sys=0
while read -r package _; do
    case "$package" in
        ring) ring=1 ;;
        aws-lc-rs) aws_rs=1 ;;
        aws-lc-sys) aws_sys=1 ;;
    esac
done

case "$profile" in
    production|ring)
        if ((ring != 1 || aws_rs != 0 || aws_sys != 0)); then
            echo "$profile requires Ring without AWS-LC; check Cargo feature unification" >&2
            exit 1
        fi
        ;;
    aws-lc)
        if ((aws_rs != 1 || aws_sys != 1 || ring != 0)); then
            echo "aws-lc requires AWS-LC without Ring; select only mcp-guest in this invocation" >&2
            exit 1
        fi
        ;;
esac
