# Linux 命令行

CLI 直接调用 Core `MonitorApp`，业务配置、商品、扫描、通知、代理和运行意愿都保存在同一数据目录中。默认目录为 `$XDG_CONFIG_HOME/ricoh-monitor`，未设置时为 `$HOME/.config/ricoh-monitor`；可用 `--data-dir PATH` 指定测试或独立实例目录。

```sh
ricoh-monitor setup
ricoh-monitor config show
ricoh-monitor config set start 09:00
ricoh-monitor products add 245
ricoh-monitor products list
ricoh-monitor products scan 120 130
ricoh-monitor notifications providers
ricoh-monitor notifications add office 办公 feishu ./channel-values.json
ricoh-monitor notifications test office
ricoh-monitor run
```

首次交互设置沿用 Core 默认值：北京时间每日 09:00–19:00、直连。需要代理时可显式启用系统代理。无终端或传入 `--json` 时直接保存默认配置。`products add` 与扫描、渠道测试、代理测试会发起对应网络请求，只有显式调用时才执行。手动加入的商品会验证后启用；扫描结果默认停用，便于用户逐项勾选，可用 `products set ID enabled|disabled` 管理。扫描检查点由 Core 保存；扫描命令运行到完成或暂停后返回。其他终端可执行 `products scan pause|resume|cancel` 控制扫描；恢复命令会继续运行到完成或再次暂停。

`config set KEY VALUE` 支持 `start`、`end`、`days`、`interval-min-ms`、`interval-max-ms`、`failures-before-backoff`、`failure-backoff-seconds`、`use-system-proxy`、`use-proxy-pool` 和 `failure-alert-after-minutes`。请求间隔默认最小 1000 毫秒、最大 2000 毫秒；`start` 与 `end` 相同表示所选日期全天监控。`config export FILE` 导出 Core 配置，`config export -` 将导出内容放入 JSON 结果；`config import FILE` 导入配置，`config defaults` 恢复默认设置。导出不包含 Secret。

通知渠道通过 JSON 文件提供 provider 字段值，例如 `{"webhookUrl":"https://...","signingSecret":"..."}`；飞书、钉钉和企业微信都要求完整 `webhookUrl`，飞书和钉钉可选 `signingSecret`，企业微信不支持签名字段。Core 验证固定 HTTPS 主机、路径和鉴权参数。使用 `notifications add ID NAME PROVIDER FILE [EVENTS]`，订阅事件可选 `stock_available,monitoring_failed,recovered`。持续有货重复提醒不属于当前 CLI/Core 渠道字段或订阅事件。Core 将凭据保存到应用 SQLite 数据库，命令输出、快照和配置导出不返回凭据。可用 `notifications enable|disable|remove ID` 管理渠道。

代理支持 `proxies add http|https|socks5 HOST PORT [AUTH-FILE]`，认证文件为包含 `username` 和 `password` 的 JSON；批量导入每行一个标准代理 URI，例如 `http://user:pass@host:port`。也可执行 `proxies test ID`、`proxies enable|disable|remove ID`。Secret 不放在命令参数或日志中。

`start`、`pause`、`resume`、`stop` 通过共享 Core 存储更新运行意愿。`stop` 停止监控意愿但不结束前台进程；Ctrl+C 或 `systemctl stop` 结束进程，随后再次启动会遵从最后保存的意愿。`status` 返回 Core 快照。成功输出为单行 JSON，错误写入 stderr。

生成普通用户 systemd 单元：

```sh
ricoh-monitor service unit --output "$HOME/.config/systemd/user/ricoh-monitor.service"
```

单元以 `%h/.local/bin/ricoh-monitor run` 前台运行，服务进程由 systemd 管理，异常退出采用有限重启策略。安装、启用和用户会话退出后的行为由用户按 Linux 发行版文档处理；本项目当前没有可用 Linux 主机，本地 Darwin 测试不能证明真实 systemd 生命周期。
