# REALITY 指纹兼容回归

使用 Python 标准库、OpenSSL 和 curl，在 loopback 启动临时 TLS cover、HTTP origin 和真实 Xray 服务端／客户端。必须收到精确 payload 才视为连通；无生产节点或外部服务依赖。

```sh
go -C components/xray-core build -buildvcs=false -o /tmp/reality-xray ./main
python3 scripts/tests/xray-reality-compat/run.py --binary /tmp/reality-xray --label default
python3 scripts/tests/xray-reality-compat/run.py --binary /tmp/reality-xray --label strict --strict
python3 scripts/tests/xray-reality-compat/run.py --binary /tmp/reality-xray --label empty --empty-cover
python3 scripts/tests/xray-reality-compat/run.py --binary /tmp/reality-xray --label strict-empty --strict --empty-cover
```

默认不填写 `allowLegacyX25519`，验证已有服务端配置仍接受 `edge / ios / qq`；`--strict` 显式设为 false，验证它们被拒绝。现代固定指纹 `chrome / firefox / safari` 在两种模式下均须成功；错误 SNI、错误 short ID 及已知不支持的 `android / 360` 均不能取得 payload。`--empty-cover` 关闭 cover ALPN，覆盖空 EncryptedExtensions 的 record 长度兼容。

每轮 14 个用例。`random / randomized` 只记录抽样，不据此宣称所有随机种子兼容；缺少初始混合 key share 的客户端修复另由 `transport/internet/reality/randomized_test.go` 验证。用 `--client-binary /absolute/path/to/old-xray` 可检查旧客户端接入新版服务端。

`--report /absolute/path/report.json` 保存脱敏结果、二进制摘要和日志目录。临时目录权限为 0700，配置权限为 0600，其中只包含本轮生成的测试密钥；保留目录便于失败诊断。夹具不能替代公网可达性、性能或机队升级验收。
