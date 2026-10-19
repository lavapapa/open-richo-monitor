# 自动检查与云端构建

GitHub Actions 承担代码检查、原生回归、发行构建和公开下载检查。普通提交进入开发验证；正式发布从版本标签进入草稿流程。

## 一、开发检查

| 流程 | 触发条件 | 检查内容 |
| --- | --- | --- |
| Quick checks | PR、main / codex 分支提交，或手动运行 | 工作流语法、桌面版本一致性、README 下载链接、发行脚本、前端与通知模块 |
| Shared desktop regression | 应用、Core、脚本、第三方资料、工作流或许可证变更，或手动运行 | Apple Silicon、Intel、Windows 原生测试和各架构 Bun 生命周期；Windows 额外运行 WebView 与原生命令场景 |
| Published downloads | GitHub 正式发布后、每周一北京时间 08:00，或手动运行 | 公开更新清单、三个安装入口及两个 Mac 更新包地址 |

文档修改仍执行快速检查，原生回归按代码路径触发。同分支新提交取消旧的开发检查。npm 与 Cargo 下载依赖由缓存复用；大型 Rust 编译目录保持临时状态，避免持续占用缓存空间。

公开下载检查在 GitHub 刚发布时检查 GitHub；每周检查及默认手动检查同时覆盖 Gitee，并核对两站版本一致。镜像附件上传完成后可立即运行：

```sh
gh workflow run downloads.yml -f include_gitee=true
```

失败原因与具体下载地址保留在运行日志和 Summary。该流程访问公开发行文件，无需账号凭据，不请求理光商城或发送聊天消息。

## 二、发行流程

`Build release` 从同一提交并行生成 Apple Silicon DMG、Intel DMG 和 Windows 安装包。发行前先检查版本、标签和更新日志；三平台文件、签名与清单相符后生成 GitHub 草稿。正式发布入口和镜像步骤见[更新与发布](updates.md#三发版)。

中间工件与 Windows 验收记录保留七天，正式 Release 附件独立保留。普通开发提交沿用 dev 验证，发布标签由维护者在确认发版后创建。

## 三、验收边界

托管环境执行自动化场景；系统横幅、扫码确认、真实消息接收、睡眠唤醒及多日使用继续按[平台验收清单](release-checklist.md)记录。发行流程生成签名更新包，操作系统发布者签名与 Apple 公证沿用平台的独立要求。
