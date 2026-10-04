# 架构决策记录

ADR 记录已经采纳、会长期影响实现或运维的决策。文件名使用 `NNNN-short-kebab-title.md`，编号递增，不重排历史。

每份 ADR 使用以下结构：

```markdown
# NNNN 决策标题

- 状态：proposed | accepted | superseded | rejected
- 日期：YYYY-MM-DD
- 负责人：<team or role>
- 替代：<ADR number, if any>

## 背景

需要解决的问题、约束和证据。

## 决策

被采纳的方案和边界。

## 后果

正面影响、代价、风险和后续义务。

## 备选方案

认真考虑过但未采用的方案及原因。

## 验证

如何证明决策仍然成立，什么信号会触发重新评估。
```

ADR 不保存任务进度，也不替代代码注释、协议规范或 Runbook。被新决策替代时保留旧文件，将状态改为 `superseded` 并相互链接。
