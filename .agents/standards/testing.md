# 验证标准

验证从最接近改动的快速检查开始，再按影响面扩大。不得声称未实际运行的检查通过。

## 选择原则

- 文档或配置：语法检查、链接/路径检查、`git diff --check`。
- 单一 Rust crate：定向测试，再运行该 crate 的 Clippy。
- 协议、公共类型或跨 crate 行为：相关 crate 测试加 workspace 检查。
- PostgreSQL 行为：普通测试加显式启用的 Docker 集成测试。
- 前端页面：相邻 Node/Vitest 测试；公共组件、令牌或路由变化扩大到完整前端检查。
- Xray fork：受影响 Go 包的 test/vet；协议、并发或数据面热路径按 CI 运行 race、fuzz-seed 和真实进程场景。
- 安装、部署或构建链：检查 `install.sh`、环境示例、目标架构和嵌入产物。

## 常用定向命令

```sh
cargo test -p brocade-core --locked
cargo test -p <crate-name> --locked <test-filter>
cargo clippy -p <crate-name> --all-targets --locked -- -D warnings
```

```sh
cd frontend
node --test tests/<name>.test.mjs
npx vitest run tests/<name>.test.tsx --config vitest.config.ts
```

```sh
cd components/xray-core
go test ./path/to/changed/package
go vet ./path/to/changed/package
```

## PostgreSQL 集成测试

这些测试需要 Docker，且必须显式开启：

```sh
BROCADE_RUN_PG_TESTS=1 cargo test -p brocade-store --test pg_integration --locked -- --ignored
BROCADE_RUN_PG_TESTS=1 cargo test -p brocade-store --test tunnel_probe_pg --locked -- --ignored
BROCADE_RUN_PG_TESTS=1 cargo test -p brocade-console --test http_integration --locked -- --ignored
```

## 标准检查

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

```sh
cd frontend
npm ci
npm run format:check
npm run lint:strict
npm test
node scripts/route-check.mjs
npm run build
```

```sh
cd ..
sh -n install.sh
git diff --check
```

完整 workspace 构建还需要 Node.js、Go、Zig、Rust 固定工具链和声明的交叉编译目标。机器缺少这些工具时，完成所有可运行的定向检查，并在交付说明中列出未验证项。

CI 的实际定义以 `.github/workflows/ci.yml` 为准；本页帮助选择本地检查，不复制 CI 的每个实现细节。
