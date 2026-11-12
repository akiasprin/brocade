# 强制格式化

## 执行时机

- 每批源码、测试、脚本或配置的新增、修改、移动、生成及合并处理完成后，Agent 必须主动运行格式化，不依赖人工提醒；编辑器、补丁和脚本写入都适用。
- 任务收尾，以及本地／KVM／GitHub CI、提交、发行构建和发布开始前，先自动格式化并执行只读检查，再冻结待验源码。失败时立即修正并重验，不先启动审计、测试或交叉编译。
- VS Code 保存时自动格式化；工具直接写文件仍须主动调用统一入口。缺少工具或版本不匹配时必须报错，不以跳过代替通过。

## 统一入口

```sh
# 每批编辑后只传本任务文件；写入后会自动再检查一次。
python3 scripts/format.py --install path/to/changed.py path/to/changed.ts

# 收尾可检查当前全部 Git 变更（包括未跟踪文件）。
python3 scripts/format.py --check

# 检查最近一次提交及工作区变更；先检 Rust 根工作区。
cargo fmt --all -- --check
python3 scripts/format.py --check --base HEAD^ --install
```

`--install` 仅将缺失的固定版本工具安装到 `.tools/format-tools`，Prettier 使用前端 lockfile 安装。提交和暂存始终由用户授权的 Git 流程执行，格式化不自动暂存或提交。

| 文件                                                                                                    | 格式化器与权威配置                                                                                                                    |
| ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| Rust                                                                                                    | 仓库钉住的 rustfmt，遵循相邻 `rustfmt.toml`；单文件不递归格式化子模块，根工作区收尾另跑 `cargo fmt --all` 和检查。                    |
| Go                                                                                                      | 使用 Xray `.go-version` 钉住的 Go 工具链中的 gofmt。                                                                                  |
| Python / stubs                                                                                          | Ruff；版本见 `format-tools.json`，风格见 `pyproject.toml`。不顺带执行 lint 自动修复。                                                 |
| POSIX sh / Bash，包括无扩展名的 Shell 脚本                                                              | shfmt，版本见 `format-tools.json`，缩进 2 空格；按 shebang 保留方言，`install.sh` 继续保持 POSIX。                                    |
| JS / TS / JSX / TSX / MJS / CJS、CSS / SCSS / Less、HTML / Vue、JSON / JSONC、YAML、Markdown / MDX、SVG | 前端 lockfile 中的 Prettier；根配置引用前端已有配置，不维护第二套取值。                                                               |
| 其他 UTF-8 文本配置，如 TOML、普通 SQL / proto / jq / env                                               | 统一入口执行 LF、无行尾空白、文件末尾换行等 `.editorconfig` 基础规范；语法与领域约束仍按对应检查验证，不宣称完成该语言的 AST 格式化。 |

SVG 显式使用 Prettier 的 HTML parser。

## 范围与不可变内容

- 默认只选择 Git 已修改及未跟踪文件，`--base` 同时纳入指定修订以来的改动，不递归遍历整个仓库。每批编辑必须显式传入本任务路径，不格式化无关工作或整个 vendored 子树。
- 自动格式化后检查 diff，保留其他工作的语义，不覆盖并行编辑。二进制、lockfile、golden、已发布迁移和存档 SQL 夹具不自动重排；需要更新不可变字节时，按对应领域规范另行处理。
- CI 的 `format` 作业先于其他作业执行。push 检查整个推送范围；首次推送分支以默认分支为基线，tag 或手动运行回退到最近一次提交，避免多提交推送只检查最后一个提交。发行构建在不可变工作树中只读检查，修正必须在冻结和启动构建之前完成。
- 浅克隆拉取默认分支时保留至少两层历史；默认分支与当前提交相同时解析实际父提交 SHA，不能把本地 `HEAD^` 表达式当作远端 ref 拉取。相同默认分支、tag 和多提交推送范围由真实浅克隆回归测试验证。

工具行为参考 [Ruff formatter](https://docs.astral.sh/ruff/formatter/)、[shfmt](https://github.com/mvdan/sh) 和 [Prettier CLI](https://prettier.io/docs/cli)。
