# Task 035: chacha20 锁文件更新 + CI package 校验

## Background
019 r3 非阻塞建议 1 & 2（docs/reviews/019-review-r3.md）：
1. `chacha20` yanked 依赖警告（依赖维护事项，不阻塞发布），正式发布前可单独更新锁文件。
2. 建议 CI/发布脚本保留干净 checkout 下 `cargo package --list` 校验，并断言 `docs/`/`.git/`/`target/` 不出现。

## Goal
（1）更新 Cargo.lock 消除 `chacha20` yanked 警告；（2）新增 CI（或发布脚本）在干净 checkout 下执行 `cargo package --list` 并断言敏感目录不出现。

## Design Notes
- 依赖更新：定位 `chacha20` 的 yanked 版本，`cargo update`（或指定 `-p chacha20`）升级到非 yanked 版本；重新跑全 feature 矩阵门禁。
- CI/脚本：新增 `.github/workflows/ci.yml`（或 `scripts/` 下脚本）执行 `cargo package --list`，断言输出不含 `docs/`、`.git/`、`target/`、`.opencode/`。
- ⚠ 归属：`.github/`/`scripts/` 不在 conventions 三方目录（src/tests/docs）内，需 PM 授权（override）后由 Developer 落库；若 PM 仅授权依赖更新，CI 部分可拆分或延后。

## Files
- Cargo.lock（chacha20 版本更新）
- .github/workflows/ci.yml 或 scripts/package-check.sh（新增，需 PM 授权）

## 错误处理
无新错误类型；CI 校验失败时应以非零退出码阻断。

## 测试要求
- 依赖更新后四门禁全绿 + 全 feature 矩阵绿。
- `cargo package --list` 输出断言（干净 checkout）。

## Acceptance Criteria
- [ ] cargo check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test --all-targets
- [ ] cargo fmt --check
- [ ] cargo package --list（断言敏感目录不出现）
