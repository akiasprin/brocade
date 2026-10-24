# 验证标准

验证以确认目标行为、关键约束和回归风险为目的，深度与范围遵循[工程哲学](../engineering-philosophy.md)的情境判断及实践与反思原则。下方矩阵和命令供选择参考，具体范围由任务决定。

## 选择原则

- 文档或配置：语法检查、链接/路径检查、`git diff --check`。
- 单一 Rust crate：定向测试，再运行该 crate 的 Clippy。
- 协议、公共类型或跨 crate 行为：相关 crate 测试加 workspace 检查。
- PostgreSQL 行为：普通测试加显式启用的 Docker 集成测试。
- 前端页面：相邻 Node/Vitest 测试；公共组件、令牌或路由变化扩大到完整前端检查。
- 前端视觉呈现：按[前端视觉验收](frontend.md#视觉验收)执行截图比较与实际看图，并配合相关行为验证。
- Xray fork：受影响 Go 包的 test/vet；协议、并发或数据面热路径按 CI 运行 race、fuzz-seed 和真实进程场景。fuzz 使用 CI 定义的固定输入数量和整进程超时，避免 Go 的计时终止竞态（[go.dev/issue/75804](https://go.dev/issue/75804)）；错误仍使检查失败，不自动重试取绿。
- Console 生成的 Xray 配置：普通测试必须执行本次 Console 构建所内嵌的 Xray；禁止隐式读取 `.tools/xray`、因本地工具缺失而跳过，或让本地与 CI 使用不同来源。只有显式开启的真实外部 E2E 可通过 `BROCADE_XRAY_BIN` 选择另一个待验产物。
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

涉及视觉呈现时，另运行 `npm run test:visual`，首次使用先执行 `npx playwright install chromium`。当前 `npm test` 不包含该入口；运行环境与报告位置见[视觉测试配置](../../frontend/playwright.config.ts)，验收要求仍由[前端标准](frontend.md#视觉验收)统一维护。

```sh
cd components/xray-core
go test ./path/to/changed/package
go vet ./path/to/changed/package
```

Node 入口中的浏览器用例串行运行，避免多个 Vite/Chromium 测试文件与构建同时抢占资源。
每个自行创建 Vite 的浏览器夹具使用独立临时缓存并在结束后删除；不要通过增加超时或重试掩盖失败。
浏览器用例与 Docker PostgreSQL 集成测试不能在同一网络命名空间同时运行：容器网卡增删会触发 Chromium 的
`ERR_NETWORK_CHANGED` 并取消本机模块请求。CI 按顺序执行；本地先结束数据库/容器操作，或将浏览器及其 Vite 服务一起置于独立网络命名空间。
Linux 支持用户网络命名空间时，可用 `unshare -Urn sh -c 'ip link set lo up && cd frontend && npm test'`，保持全部断言且不依赖宿主机网卡状态。

Console 升级发布必须通过[官方 v0.3.1 强制真实验收](../../scripts/tests/upgrade/README.md)，包括
外置与受管 PostgreSQL。CI 使用同次发行构建的产物，不允许仅用模拟服务或空匹配结果代替。

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
