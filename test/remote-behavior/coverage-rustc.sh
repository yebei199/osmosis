#!/bin/sh
# RUSTC_WORKSPACE_WRAPPER：只给工作区 crate 插桩，依赖不插，免得软件渲染再慢一截。
# 计数器重定位让 LLVM_PROFILE_FILE 的 %c 连续模式生效，被 SIGKILL 的进程也留下计数。
case " $* " in
*" --crate-name build_script_build "* | *" -vV "*) exec "$@" ;;
esac
exec "$@" -C instrument-coverage -C llvm-args=-runtime-counter-relocation
