#!/usr/bin/env bash
# Task 035：干净 checkout 下校验发布包白名单。
#
# 断言 `cargo package --list` 的输出不含敏感目录（docs/、.git/、target/、.opencode/）。
# 发布包只应包含运行/构建所需文件（见 Cargo.toml `include`）；内部开发文档
# （docs/ 任务板、审查报告）与本地状态（.git/、target/、.opencode/）不得进入包。
# 校验失败以非零退出码阻断（CI 中即红）。
set -uo pipefail

# 发布包不应包含的路径前缀（相对包根）。
FORBIDDEN=(
  "docs/"
  ".git/"
  "target/"
  ".opencode/"
)

# 优先走干净 checkout 路径（CI 恒为干净）；本地 dirty 工作区回退 --allow-dirty，
# 使脚本在两种场景下都可运行。--allow-dirty 不改变 include 模式决定的路径集合，
# 仅允许在 dirty 状态下执行。
list_output="$(cargo package --list 2>&1)"
rc=$?
if [ "$rc" -ne 0 ]; then
  list_output="$(cargo package --list --allow-dirty 2>&1)"
  rc=$?
fi
if [ "$rc" -ne 0 ]; then
  echo "package-check: ERROR — cargo package --list 失败" >&2
  printf '%s\n' "$list_output" >&2
  exit 1
fi

status=0
for pattern in "${FORBIDDEN[@]}"; do
  if grep -qF -- "$pattern" <<<"$list_output"; then
    echo "package-check: ERROR — 发布包包含敏感路径: $pattern" >&2
    grep -F -- "$pattern" <<<"$list_output" >&2
    status=1
  fi
done

if [ "$status" -ne 0 ]; then
  echo "package-check: FAILED" >&2
  exit 1
fi

echo "package-check: OK — 发布包不含 docs/、.git/、target/、.opencode/"
