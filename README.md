# open-richo-monitor

RichoMonitor 是在用户设备上运行的理光库存提醒工具，代码仓库名为 open-richo-monitor。macOS、Windows 桌面版与 Linux 命令行版共享监控核心；用户选择商品后，程序按时间计划检查上架和库存，通知渠道可按需配置。

## 一、桌面使用

[下载安装包](https://github.com/lavapapa/open-richo-monitor/releases/latest)，或使用 [Gitee 镜像](https://gitee.com/marvinfore/open-richo-monitor/releases/latest)。Mac 提供 Apple Silicon 与 Intel 版本，需要 macOS 13 或更新版本；Windows 提供 x64 安装程序。应用沿用已有商品、配置和监控历史。首次引导默认选择官翻 GR III / IV 系列，通知步骤可以跳过；已有用户可在“其他设置 → 帮助引导”重新设置。

Mac 拖入 Applications 后打开，Windows 运行安装程序。当前采用 Mac 本地签名，Windows 安装包未做 Authenticode 签名，首次安装按系统提示确认可信来源。应用启动和运行期间每小时检查更新；发现新版后可在界面点击“下载并重启”。

监控默认使用全站上架列表，在“其他设置 → 监控方案”可切换逐商品详情。列表完整分页后同步已选商品，未见商品的库存保持未知；商品验证和手动扫描使用详情接口。为已监控商品开启“突出提醒”后，上架、恢复有货或库存增加时会出现桌面覆盖图层，可按 Esc、空格或回车关闭。“通知”页系统通知下方提供本地演示。

库存请求默认直连；需要代理时，可在“代理池”显式启用系统代理。添加商品会向理光发送请求，外部通知渠道测试会发送真实测试消息，系统通知测试会调用当前平台的通知接口。连接错误可查看并复制原始诊断。开始监控前需启用至少一个有效商品并完成设置。

通知支持飞书、企业微信、钉钉和微信。点击平台按钮后扫码，在手机上完成连接，应用默认选择已识别的个人会话；保存会发送测试消息，新渠道测试成功后自动启用。“接收位置”可选择个人与多个群聊，编辑时也可刷新并调整。飞书直接查询机器人所在群，企业微信与钉钉需要先在群内 @机器人发送一条消息以识别该群，微信提供个人会话。

Linux 命令行端从 `crates/cli` 构建。

## 二、开发

开发环境需要 Rust、Node.js 与 npm；通知模块还需要 [构建入口](scripts/build-notification-runtime.mjs) 指定的 Bun 版本。macOS 需要 Xcode Command Line Tools，Windows 需要 MSVC 构建工具与 Windows SDK。在 apps/desktop 与 apps/notification-runtime 分别执行 npm ci 后，可通过 `RM_BUN_PATH` 指定 Bun 可执行文件，再运行 [scripts/dev-macos.command](scripts/dev-macos.command)；它使用独立的调试版应用标识。发行 App 已内置通知模块，用户无需安装运行时。

前端检查在 apps/desktop 执行 npm run check、npm test；通知模块测试在 apps/notification-runtime 执行 npm test。核心测试执行 cargo test --manifest-path crates/core/Cargo.toml；桌面原生测试执行 cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml。Mac 打包在 apps/desktop 执行 `npm run package:macos:dmg`，使用当前 Mac 的原生架构；此入口将自包含通知脚本与原厂签名 Bun 打入 App，并验证嵌套签名。发行构建需要配置 Tauri 更新签名环境变量，详见 [更新与发布](docs/updates.md)。

Windows 开发和打包也通过 `RM_BUN_PATH` 指定同一冻结版本的 Bun；调试入口为在 apps/desktop 执行 `npm run tauri -- dev --config src-tauri/tauri.windows-x64.json`。Windows 打包入口为 [scripts/build-windows.ps1](scripts/build-windows.ps1)。在 Windows 的项目根目录执行 `powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1`，脚本安装与测试通知模块及桌面依赖，执行共享测试，并将通知运行时与脚本打入 NSIS 安装程序，输出到 `dist/windows-x86_64/`。原生平台验收场景见 [testing.md](docs/testing.md)。

## 三、约定

[文档索引](docs/README.md)汇总当前契约、架构与测试方法，界面约定见 [DESIGN.md](DESIGN.md)。macOS 产物使用本地签名，Windows 安装包尚未进行发布签名；正式分发需分别验收。

正式发布前检查 [macOS 发版验收清单](docs/release-checklist.md)。项目采用 [MIT 许可证](LICENSE)，第三方代码及资产沿用各自许可证。
