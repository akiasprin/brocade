# Console 升级验收

以下各项互补：Docker 数据库实验验证真实 SQL 和压缩逻辑备份；安装器容器测试验证文件切换；
KVM 演练验证官方旧程序、真实 systemd、整机重启及恢复后登录。都不访问生产环境。

## 强制发布门槛

本次只将官方 v0.3.1 作为升级验收基线。CI 的 `mandatory v0.3.1 real upgrade and restore` job
在四平台发行构建完成后执行，与常规检查并行，使用同一次构建的 GNU x86_64 发行包，固定旧版发行包 SHA-256，
不接受替代旧程序或未校验的操作系统镜像。缺少 KVM、验收失败或缺少最终结果都会使 CI 失败。

CI 保存实际 systemd 升级、备份恢复、重复升级、两种数据库模式各自整机重启后登录及数据保留证据；正式发布须以该
job 与同提交所有常规检查成功及对应提交的发行物为依据。旧代码恢复只发生在隔离验收环境，不用于覆盖生产新写入。

## KVM/systemd 升级与恢复演练

需要 Python 3.11+、可用 `/dev/kvm`、带 `qemu-system-x86_64` / `qemu-img` / `cloud-localds` 的容器镜像，
已校验的 Ubuntu 24.04 amd64 cloud image，以及校验过官方发布摘要的 v0.3.1 GNU
发行压缩包。虚拟机从只读基础镜像创建独立 overlay，联网安装 PostgreSQL 16 客户端和服务端；
受管模式仍使用 launcher 自己校验下载的 PostgreSQL 17.11。

```sh
python3 scripts/tests/upgrade/run_kvm.py \
  --console target/debug/brocade-console \
  --launcher target/debug/brocade-launcher \
  --release-031 /path/to/brocade-dist-0.3.1-linux-x86_64-gnu.tar.gz \
  --vm-image /path/to/verified-ubuntu-24.04-amd64.qcow2 \
  --vm-image-sha256 <Ubuntu 官方 SHA256SUMS 核验后的摘要> \
  --output .tools/upgrade-kvm-drill
```

| 演练 | 成功判据 |
| --- | --- |
| 官方 v0.3.1 × 两种数据库正常升级 | 0001→候选的完整迁移链，旧管理员可登录，配置、账户、密钥、接入/机器额度、用量/计数头及旧 checksum 保留，未创建二进制发布单 |
| 压缩备份恢复 | gzip 校验通过，外部逻辑备份有压缩；恢复原安装和数据库后，官方旧版能启动并登录 |
| 迁移前拒绝及备份失败 | 未知 checksum、较新迁移、缺 pg_dump、压缩失败时不修改 schema，旧服务可用 |
| 非超级用户备份权限 | 外置库用普通应用角色；管理员归档表仅授予读取，撤销 schema USAGE 后备份失败，旧服务恢复且未开始迁移 |
| 迁移中失败 | 注入真实 DDL 冲突后整条事务回滚，保留备份及开始标记，服务停服 |
| 升级后启动失败 | 已迁移时保持停服待修复；没有待应用迁移时恢复旧安装及服务 |
| systemd socket 传递及普通重启 | Console 与 launcher 共享实际监听 FD；普通 service 重启后 socket InvocationID 保持，原数据可读 |
| socket 部署再次完整升级 | 真实 pg_dump 执行时 socket/service 均为停止，防止自动激活写库；升级后监听恢复 |
| 重复升级及整机重启 | 不重复迁移；虚拟机 boot ID 改变后，systemd 自动拉起服务并读到原数据 |

启动失败由真实 systemd 拒绝启动或仅在隔离测试包中的 launcher 包装脚本注入，数据库维护
仍执行真实候选命令。压缩失败由虚拟机内仅用于该次安装进程的 PATH 包装脚本注入。
结果及摘要在输出目录；每组恢复核验后清理上一组的数据库和备份，以限制磁盘占用。
最终场景的压缩备份位于隔离 guest 的 `/var/backups/brocade-upgrades`，随 overlay 保留。
测试结束删除本次 QEMU 容器；失败现场的 overlay 和日志保留，避免影响其它实验环境。
guest 脚本带显式标记和 KVM 检查，禁止直接在宿主机或线上运行。

## 定向数据库及安装器实验

只使用显式指定的本地候选二进制和新建的 Docker PostgreSQL 17，不访问生产环境。
需要 Docker、兼容 PostgreSQL 17 的 `pg_dump` / `pg_restore`，以及已编译的候选：

```sh
python3 scripts/tests/upgrade/run.py \
  --console target/debug/brocade-console \
  --launcher target/debug/brocade-launcher \
  --output .tools/upgrade-acceptance
```

初始化与官方 v0.3.1 字节一致的 0001 和真实迁移回执，调用真实预检、压缩备份及迁移命令，
验证 0002 生效、配置和旧 checksum 保留，并把升级前逻辑备份恢复到另一个空库核验。
同时核对定制格式备份内部使用 gzip 压缩。随后注入真实 DDL 冲突，核验整条迁移回滚、旧回执
不变、备份和迁移开始标记保留。安装器文件备份及受管冷备份均为 `.tar.gz`。
这项实验不宣称真实 systemd、其他发行版或 PostgreSQL 大版本升级已验收。

Launcher 的普通测试另覆盖安装器的入口保留、迁移前恢复旧服务、迁移开始后拒绝回退，
以及备份/迁移失败边界。数据库失败回执、未知历史和较新迁移的拒绝由 Store PostgreSQL
集成测试覆盖。

安装器的五种文件切换/失败恢复场景可独立执行，容器内模拟 systemctl，不操作宿主机服务：

```sh
BROCADE_RUN_UPGRADE_INSTALLER_TESTS=1 \
  python3 -m unittest discover -s scripts/tests/upgrade -p 'test_*.py' -v
```

受管模式使用内置 PostgreSQL 17.11，检查运行中旧库、停服冷备份、迁移同一个集群，并将冷备份
恢复到隔离目录重新启动验证（需普通用户和已编译的显式 Console 候选）：

```sh
BROCADE_UPGRADE_CONSOLE_BIN="$PWD/target/debug/brocade-console" \
  cargo test -p brocade-launcher --locked managed_upgrade_checks -- --ignored
```

一次性手动部署纳管可额外传 `--adoption-script /path/to/adopt-runtime.py`，在同一隔离虚拟机用真实服务、非空数据库和原密钥演练，再走一次实际安装器升级。此操作脚本只用于明确的生产纳管，不改变官方 v0.3.1 兼容基线。
