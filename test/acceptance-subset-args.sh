#!/usr/bin/env bash
# #187 AC-1：acceptance/run.sh subset 对 JUnit ID 的转换；只测不起构建环境的那几条路。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
got=$(bash acceptance/run.sh subset-args test_x::test_y app-core::blocks::t test_a.py::test_b 'test_p::test_q[a-b]')
want=$'test_x.py::test_y\ntest_a.py::test_b\ntest_p.py::test_q[a-b]'
[[ "$got" == "$want" ]] || { echo "FAIL convert: $got" >&2; exit 1; }
out=$(bash acceptance/run.sh subset app-core::blocks::t ui-x::y) || { echo "FAIL all-skipped exit $?" >&2; exit 1; }
[[ "$out" == *"nothing to run"* ]] || { echo "FAIL all-skipped output: $out" >&2; exit 1; }
echo ok
