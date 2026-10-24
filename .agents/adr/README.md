# 架构决策记录

ADR 保存长期影响实现或运维的重要决策，使后来者能够理解当时的背景、依据和后果。文件名使用 `NNNN-short-kebab-title.md`，编号递增，不重排历史。

分析深度遵循[提案规范](../standards/design-planning.md)。以下结构供参考，可按决策需要调整，并引用已有研究和计划。

```markdown
# NNNN 决策标题

- 状态：proposed | accepted | superseded | rejected
- 日期：YYYY-MM-DD
- 负责人：<team or role>
- 替代：<ADR number, if any>

## 背景与目标

需要解决的问题、重要约束和期望达到的状态。

## 决策与依据

选择的方向及理由，包括对判断有实质影响的研究和方案取舍。

## 后果与验证

预期影响、接受的代价、重要风险，以及如何验证和重新评估。
```

ADR 保留决策依据，执行进度放在计划中。被新决策替代时保留历史、更新状态并相互链接。
