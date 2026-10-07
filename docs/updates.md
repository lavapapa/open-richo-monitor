# 更新与发布

桌面端通过 Tauri 官方 updater 读取 GitHub Release 的 `latest.json`。启动时检查，运行期间由原生任务每小时检查；睡眠后跳过积压的检查。发现新版本后显示提示，用户点击“下载并重启”才开始下载和安装。

检查与下载使用官方插件内置的系统静态 HTTP/HTTPS 代理支持，独立于商城请求的代理池开关。PAC 与自动发现代理暂未支持，检查失败时可从项目发布页手动下载安装包。

## 一、生命周期

检查和下载期间继续监控。下载完整且通过 Tauri 的更新签名验证后，使用平台默认安装流程：Mac 替换整个 App 并重新启动，Windows 使用默认 passive 模式启动 NSIS 安装器。Windows 安装器直接退出进程，`updates.rs` 的 `on_before_exit` 会先收尾监控、保存通知会话并退出 Bun；Mac 重启沿用应用的退出收尾。更新保留应用标识和用户数据目录，启动后按原设置恢复监控和渠道连接。

下载或更新包验证失败时保留原应用与连接，界面提供重试。Windows 解包失败时保留原应用；启动安装器失败时重新启动原应用恢复运行。安装器启动后的失败或取消由安装器报告，必要时手动重新打开应用或从发布页安装完整安装包。安装期间监控短暂中断，该时段发生的瞬时库存变化可能无法补查。应用安装在只读磁盘映像时，先拖入可写安装目录再使用更新。

## 二、构件

每个版本分别发布 Apple Silicon、Intel Mac 与 Windows x64。`scripts/create-update-manifest.mjs` 从配置版本和实际 `.sig` 生成平台条目。Mac 更新使用完整 `.app.tar.gz`，Windows 更新使用同一 NSIS `.exe`；DMG 供手动安装。架构选择由官方插件完成。

原生程序、通知脚本、Bun 可执行文件与第三方声明作为一个安装单元交付。更换 Bun 时按 `scripts/build-notification-runtime.mjs` 的冻结版本更新源码资料，并在目标架构执行 `scripts/test-notification-runtime.mjs`。Mac 构建通过 `scripts/build-macos.mjs` 恢复 Bun 原厂签名、签外层 App，然后重新生成并签署更新归档；不得使用恢复签名前的归档。

## 三、发版

在 `apps/desktop/src-tauri/tauri.conf.json` 更新版本，同时同步 desktop 的 Cargo/npm 版本，提交并建立同名 `v版本` 标签。读取 [验收清单](release-checklist.md)，完成相关平台验收；通过 `.github/workflows/release.yml` 构建三个平台并上传到草稿 Release。确认全部安装包、签名与更新清单可用后发布该 Release，并将其设为 latest。更新清单与构件应一次发布，避免指向尚未上传的文件。

更新私钥保存在发布者的受限本地目录与 GitHub Actions 的 `TAURI_SIGNING_PRIVATE_KEY` secret，密码使用 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`；源码中的 updater `pubkey` 用来验证更新。妥善备份私钥，沿用同一密钥签后续版本。更新签名独立于 macOS Developer ID 和 Windows Authenticode；操作系统首次安装限制仍按平台规则处理。

Bun 所含 LGPL 组件的源码资料通过 [SOURCE.txt](../third_party/bun/SOURCE.txt) 获取，安装应用无需下载资料。每次更换其版本时确认资料完整与可达。

## 四、验收

使用隔离的旧版本安装目录验证“发现更新、下载、安装、重启”，检查原数据、监控意图、通知会话、系统权限与 Bun 版本。用本地可控服务器覆盖无更新、断网、下载中断、无效签名、并发点击与平台缺失；测试服务器配置独立于公开构件。公开后分别从 Mac、Windows 读取真实 HTTPS 更新清单，确认同版不会提示更新。

公开发布遇到问题时提供手动安装入口，后续修复以更高版本发布；默认版本比较阻止降级。修改数据库结构时另验迁移与安装中断，应用文件恢复不会同步撤销数据库变更。无需为此维护单独的更新服务。
