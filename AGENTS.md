# openCompare Agent 开发入口

本文件是项目开发与 AI 工作流的统一入口。规范、计划、验收记录和文档检查工具集中在 docs/dev/；本文件仅维护导航与加载方式。

## 加载方式

修改本项目、评审或维护计划时，使用 [opencompare-development skill](.agents/skills/opencompare-development/SKILL.md)，加载 [AI 开发工作流](docs/dev/ai.md)，再按下表读取任务相关规范。无法自动发现 skill 时直接读取工作流文件；已读取且未变化的内容无需重复加载。

## 统一文档导航

按任务读取相关文件与章节；每类内容只在下表指定位置维护。

| 任务或内容 | 规范与事实来源 |
| --- | --- |
| AI 开发、修复与评审流程 | [AI 开发工作流](docs/dev/ai.md) |
| 领取任务、核对依赖、更新完成状态 | [开发计划](docs/dev/plan.md) |
| 功能范围、工作流、视觉与可访问性 | [产品需求](docs/dev/product.md) |
| 目录、技术栈、状态归属、IPC、文件保护与隐私 | [技术架构](docs/dev/architecture.md) |
| 开发环境、命令、协作、分支与交接 | [开发指南](docs/dev/guide.md) |
| 测试、共享样本、性能与完成条件 | [质量标准](docs/dev/quality.md) |
| 打包、系统支持、签名与发布 | [发布指南](docs/dev/release.md) |
| 架构、工程与产品审查结论 | [项目审查](docs/dev/review.md) |
| 实际验收结果与限制 | [验收记录](docs/dev/validation.md) |
| 原型维护、启动与历史功能边界 | [旧原型说明](docs/dev/legacy.md) |
| 文档链接、任务依赖与完成标记检查 | [文档检查脚本](docs/dev/scripts/check-docs.mjs) |
| 面向用户的安装与使用说明 | [README.md](README.md) |
