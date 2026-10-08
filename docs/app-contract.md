# 共享应用契约

本契约是桌面端与 Linux CLI 共用的 Rust/Core 边界。Core 实现业务状态与配置；平台入口负责参数输入、事件输出和操作系统能力。字段采用 serde `camelCase`，命令返回业务结果或可直接呈现的错误，不暴露内部 revision、序号门槛、凭据引用或存储结构。

## 监控方案

配置 `monitoringMode` 取 `listed_products` 或 `product_detail`，默认 `listed_products`。列表模式每出口一条串行任务，分页到空页后对所有已选商品各提交一次检查；详情模式每商品、每出口一条线路。两种方案共用计划、错误、统计、通知与持久化用例。随机间隔从完成时刻计时：列表模式在整轮分页完成后等待，详情模式在单次请求完成后等待；范围允许从零开始，0～0 表示无额外等待。实际节奏由配置等待、请求耗时、并发和服务端冷却共同决定。

列表模式每页请求 20 件，完整轮次最多 1000 件、51 页、60 秒。后页失败或结构异常放弃整轮结果，保留最近成功记录。完整轮次缺席的商品库存为 `None`，出现且库存为零的商品为 `Some(0)`；两者在观察、历史和界面中保持区分，缺席不更新元数据。配置方案变更推进商品代际，在途旧结果即使经过 A→B→A 切换也不能提交。分页限制与库存语义见[列表监控约定](list-monitoring-workplan.md)。

## 长期运行约束

Core 的请求序号、活跃失败累计时长和通知待投递记录持久化；网络任务重建及进程重启沿用已提交状态，关机、暂停和计划外时段不增加失败累计时长。显式取消某件商品结束该商品健康周期，其他商品保持各自状态。请求序号覆盖成功与失败检查，事件编号在清空记录后保持单调。

商品验证、扫描和普通监控共用并发准入与服务端冷却；扫描自身间隔及暂停等待在准入队列外执行。调度器沿用共享请求门控的单调时钟，任务恢复或重建保持同一截止时间基准；列表准入因计划结束或用户停止而中断时放弃未完整读取的结果，不记录接口失败。Core 维护运行任务健康，任务异常结束发布 `worker_failed` 和具体诊断；既有库存观察保留其成功时间，界面显示该数据的历史属性。

消息查询由 Core 按北京时间日期和商品先过滤再分页，每页最多 100 条；前端保持单页数据，阅读旧页时保持当前位置。状态变化和提醒分别保留 30 天、最多 10000 行；每新增一条，在提交事务内最多裁掉一条最旧历史，数量上限与写入速率无关，既有超限数据继续分批回收。商品目录最多保存 1000 件，新增或更新的名称最多 1024 字节；超过限制返回明确错误，事务保持完整。检查次数在 SQLite 整数上限饱和，保持整数类型。

已移除商品在历史、通知待投递和可恢复扫描均无引用后，按维护批次回收其身份、健康状态与轮次记录；再次加入沿用全局单调轮次发号，旧请求无法写入新周期。正在扫描的范围内，移除尚未入库的商品同样会阻止该扫描重新加入它。

通知事件与对应的待投递记录在同一事务中提交；群通知和系统通知各自消费，操作系统通知提交等待不阻塞界面快照。待投递状态覆盖已接受、失败、结果未知及过期，网络结果未知时保持诊断并避免自动重复发送。维护每轮清理固定批次并执行被动 WAL 检查点，监控结果快照按 500 毫秒合并，用户操作仍立即返回最新状态。

配置文件导入最大 4 MiB，读取前检查大小；桌面日志按 1 MiB 轮转并保留一个上一份日志，单条日志最大 64 KiB。历史数量上限约束记录数，SQLite 文件可能保留可复用的空闲页，被动检查点也会受长时间读事务影响。

## Core 服务

引导和突出提醒的事务、上架判定、消息前缀、过期清理与平台展示边界见[引导与突出提醒契约](onboarding-alert-core.md)。首次引导批量保存商品选择，通知设置为可选项；桌面设置页提供再次引导入口。

公开类型位于 `ricoh_monitor_core::app`。`MonitorApp::open` 同步构造并持有唯一 SQLite owner；其余网络/运行操作为 async：

```rust
MonitorApp::open(data_dir: impl AsRef<Path>) -> Result<MonitorApp, AppError>
app.snapshot() -> Result<AppSnapshot, AppError>
app.query_messages(query: MessageQuery) -> Result<MessagePage, AppError>
app.save_config(config: AppConfig) -> Result<(), AppError>
app.apply_startup_monitoring() -> Result<(), AppError> // 桌面新进程启动时调用
app.set_auto_start_monitoring(enabled: bool) -> Result<(), AppError>
app.set_system_notifications_enabled(enabled: bool) -> Result<(), AppError>
app.monitoring_action(action: MonitoringAction) -> Result<(), AppError>
app.validate_product(product_id: u64) -> Result<ProductRecord, AppError>
app.add_product(product_id: u64) -> Result<ProductRecord, AppError>
app.set_product_enabled(product_id: u64, enabled: bool) -> Result<(), AppError>
app.set_onboarding_products(product_ids: &[u64]) -> Result<(), AppError>
app.set_product_prominent_alert(product_id: u64, enabled: bool) -> Result<(), AppError>
app.claim_prominent_alert() -> Result<Option<ProminentAlert>, AppError>
app.acknowledge_prominent_alert(event_id: i64) -> Result<(), AppError>
app.remove_product(product_id: u64) -> Result<(), AppError>
app.start_product_scan(start_id: u64, end_id: u64) -> Result<ProductScan, AppError>
app.control_product_scan(action: ScanAction) -> Result<ProductScan, AppError>
app.wait_for_scan() -> Result<ProductScan, AppError> // 等待当前进程启动的扫描worker暂停或结束，供 CLI 前台显示结果
app.save_channel(channel: ChannelInput) -> Result<ChannelView, AppError>
app.test_channel(id: &str) -> Result<ChannelTest, AppError>
app.set_channel_enabled(id: &str, enabled: bool) -> Result<(), AppError>
app.remove_channel(id: &str) -> Result<(), AppError>
app.add_proxy(proxy: ProxyInput) -> Result<ProxyRecord, AppError>
app.import_proxies(text: &str) -> Result<ProxyImportResult, AppError>
app.test_proxy(id: &str) -> Result<ProxyRecord, AppError>
app.set_proxy_enabled(id: &str, enabled: bool) -> Result<(), AppError>
app.remove_proxy(id: &str) -> Result<(), AppError>
app.export_config() -> Result<String, AppError>
app.import_config(text: &str) -> Result<(), AppError>
app.restore_defaults(clear_history: bool) -> Result<(), AppError>
app.subscribe() -> broadcast::Receiver<AppSnapshot>
app.run() -> Result<(), AppError>
```

`MonitoringAction` 为 `Start | Pause | Resume | Stop | Restart`，`ScanAction` 为 `Pause | Resume | Cancel`。Core 存储单一 `RunIntent { Stopped, Running, Paused }`，新库默认 `Stopped`；`Start/Resume/Restart` 写 `Running`，`Pause` 写 `Paused`，`Stop` 写 `Stopped`。桌面常驻进程及 Linux 前台 `run` 持有运行循环；独立 CLI 控制进程直接读写同一 SQLite intent，status 读取同一运行快照，不通过 socket、JSON-RPC 或临时 IPC。`run` 与 systemd 自动重启读取并遵从意愿。桌面新进程由 `MonitorApp::apply_startup_monitoring` 应用 `AppConfig.autoStartMonitoring`：开启且已完成设置时写入运行意愿，关闭时以停止状态打开；运行仍受监控计划约束。关窗、托盘重新显示和重复启动不再次应用该偏好，暂停状态因此保留。`set_auto_start_monitoring` 保存未来启动偏好，当前运行意愿保持不变；登录后启动由操作系统独立管理。正常退出保持当前意愿及有效待发送记录。商城 HTTP 客户端按 `AppConfig.useSystemProxy` 配置系统代理或直连，通知使用独立的 `notificationUseSystemProxy`；Windows 库存、图片及通知请求按实际目标复用 WinHTTP 的 PAC、WPAD 与静态代理解析，解析在可取消的宿主子进程中执行并计入请求时限；其他系统沿用各自配置读取。系统通知权限、登录启动等 OS 能力仍属平台适配。

`RuntimeSnapshot.lastSuccessAt` 表示当前已启用商品最近一次持久化成功观察，直接由这些商品的 `observation.checkedAt` 求最新值。暂停或重启后保留已有成功时间，未取得成功观察时为空；停用商品不参与该值。Core 快照提供这一事实，前端无需自行重建或缓存。

完成设置需要至少一件已启用且验证过的商品，用户仍须明确确认设置后才能开始监控。系统通知和 Webhook 渠道均可稍后配置；没有通知方式时，库存检查与本地记录照常运行。

Core 服务拥有配置、商品/库存状态、通知渠道与发送、扫描、监控生命周期和持久化。商品列表中 `ProductRecord.enabled` 是唯一启停事实来源，`AppConfig` 不保存 `selectedProductIds`；渠道 `NotificationChannelView.enabled` 是唯一启停事实来源，配置不保存 `enabledChannelIds`。导出的配置包含每个商品/渠道一次，不另附选中 ID 数组。扫描与监控共用 Core 网络客户端和限流。上述方法中需 OS 或发布服务的主页链接、教程、更新检查、登录启动、日志目录、诊断保存由 Tauri 的平台命令提供，不假设这些都是 Core 能力。

配置导入在单个 SQLite 事务中完成；无效输入不留下部分修改。导出不包含凭据，导入到没有原凭据的设备时仍保留渠道名称和订阅，渠道保持待配置、未测试且停用。已有同名且同平台的本机凭据可保留，渠道仍需重新测试后启用。

Core 以 `AppSnapshot` 作为当前业务状态载体，历史消息由 `query_messages` 分页返回。`subscribe()` 返回 `tokio::sync::broadcast::Receiver<AppSnapshot>`，状态变化发送完整快照，不另设增量事件协议或顺序号。Tauri 转发快照到 `desktop-state-changed`；平台字段用 serde `flatten` 把 `AppSnapshot` 与 `PlatformSnapshot` 合成单个桌面 payload。CLI直接订阅或在终端渲染。`wait_for_scan()` 等待当前 MonitorApp 实例的扫描worker；扫描暂停、恢复、取消通过持久化控制状态支持跨进程操作。

## 公共 JSON 字段

`AppConfig` 是 Core 定义的唯一可编辑业务配置，字段以 [app.rs](../crates/core/src/app.rs) 的 `AppConfig` 为准。`autoStartMonitoring` 是持久化的应用启动偏好，与当前运行意愿及操作系统登录启动分别存储；默认值由 `MonitorConfig::default` 定义。计划起止时间相同时表示所选日期全天监控。商品勾选写 `ProductRecord.enabled`，渠道启停写 `ChannelView.enabled`，计划只存 `days`；Core 负责唯一校验和内部 `MonitorConfig` 转换。

`AppConfig::default()` 从 `MonitorConfig::default()` 转换；默认计划、请求间隔、失败等待与提醒参数以该定义为准。商品按配置的目标节奏运行，扫描还受自身发送间隔约束，两者共用 `RequestGate` 的并发准入与冷却。系统通知偏好由 Core 单独存储，默认开启并出现在 `AppSnapshot.systemNotificationsEnabled`。内部并发上限和超时沿用 Core 默认，不暴露 UI。商品 ID 和启用状态只在 `products` 行保存，渠道启用状态只在 `channels` 行保存。

商城系统代理由用户显式启用，已有配置保持用户选择；商品验证、扫描及监控遵守保存的网络模式，启用代理池时通过可用池出口请求，空池保持等待或返回明确不可用错误。通知通过独立的 `AppConfig.notificationUseSystemProxy` 控制，位于其他设置，默认开启；系统未配置适用代理时自然直连，关闭该选项时通知直连。通知独立于商城代理开关与代理池，开关通过单字段命令保存；整页监控配置保存期间暂停该开关，避免旧配置覆盖刚保存的选择。专用通知子进程移除继承的代理及绕过环境变量，由宿主传入实际系统线路；线路变化时，已保存账号与扫码临时账号均重建连接。macOS 通知目前使用静态 HTTPS 代理，PAC、自动发现及 SOCKS-only 配置会明确报错，系统绕过列表尚未传入通知运行时；Windows 通知按目标解析原生系统代理。连接超时包含 DNS、代理隧道和 TLS 建立时间；错误详情保留原始错误链，并附网络模式、阶段、耗时与超时上限。网络失败按配置等待后继续检查，显式代理失败保持该出口，不自动切换路线。外部代理或服务故障的诊断与本项目请求构造、调度故障分开验收。

`AppSnapshot` 中的 `runtime` 使用现有 `RuntimeSnapshot` 字段：`state`、`nextStartAt`、`lastSuccessAt`、`lastError`。商品记录含 `productId: string`、`name`、`enabled`、`checkCount`、`observation`、`runtimeError`；`ProductObservation` 含最近一次有效观察的库存状态、`isShow`、数量和时间，当前请求错误只通过 `ProductRecord.runtimeError` 提供。渠道记录含 `id`、`name`、`providerId`、`providerName`、`enabled`、`configuredFieldKeys`、`subscriptions`、`lastTest`、`lastDelivery`，不含 Secret 或其存储引用。`lastDelivery` 为最近一次投递的 `{ outcome, event, at, message }`，outcome 为 `accepted | failed | unknown`。代理记录含 `id`、`protocol`、`displayAddress`、`enabled`、`status`。扫描记录的 `currentId` 仅在当前请求进行时有值，结果为未选中的商品记录。最近状态变化由 `recentChecks` 呈现，包含商品、`isShow`、库存和毫秒时间，独立于通知订阅保存。`recentEvents` 仅记录有货、持续失败和恢复等提醒事件；持续有货本身不重复提醒。

Rust 类型由 Core 定义并直接 serde；TypeScript 类型逐字段描述同一 JSON 契约，但 Tauri 不再定义 `MonitoringConfigInput/Output`、`DesktopSnapshot`、`ProductRecord` 等副本或执行配置字段映射。平台字段不混入业务配置：系统通知权限、登录启动状态、日志目录及发布链接由桌面入口提供一个可选 `platform` 扩展；Linux CLI 不要求这些字段。

消息历史通过 `query_messages` 独立查询。`MessageQuery` 提供北京时间日期、商品编号、游标和页大小；Core 在 SQLite 中筛选后按时间、来源和来源内 ID 稳定排序。`MessagePage` 合并状态变化和提醒，包含当前页与下一页游标；同次有货状态变化与提醒合为一条。历史保留范围见本文件“长期运行约束”。快照内的最近记录用于简要状态；通知消费独立的持久化待投递记录。

公共定义完整字段如下：`AppSnapshot { setupCompleted: bool, systemNotificationsEnabled: bool, systemNotificationDelivery: Option<ChannelDelivery>, runtime: RuntimeSnapshot, config: AppConfig, products: Vec<ProductRecord>, catalog: Vec<ProductRecord>, channels: Vec<ChannelView>, providers: Vec<ProviderDefinition>, proxies: Vec<ProxyRecord>, scan: Option<ProductScan>, recentEvents: Vec<RecentEvent>, recentChecks: Vec<RecentCheck> }`。`RuntimeSnapshot { state: RuntimeState, nextStartAt: Option<String>, lastSuccessAt: Option<String>, lastError: Option<String> }`；状态值为 `monitoring | partial_error | outside_schedule | paused | setup_incomplete | stopped | worker_failed`。`ProductRecord { productId: String, name, enabled, checkCount, observation: Option<ProductObservation>, runtimeError }`；`ProductObservation { availability, isShow, stock, checkedAt }`。`ChannelView { id, name, providerId, providerName, enabled, configuredFieldKeys, subscriptions, lastTest, lastDelivery }`；`ProviderDefinition { id, name, documentationUrl: Option<String>, fields: Vec<ProviderField> }`，字段为 `key,label,type,required,placeholder,help`。`ProxyRecord { id, protocol, displayAddress, enabled, status }`。`RecentEvent { id: i64, at, kind, productId, message }`；`RecentCheck { productId, name, availability, isShow, stock, at }`。

枚举以 camelCase 对象字段、snake_case 字符串值序列化。`Weekday` 值为 `mon | tue | wed | thu | fri | sat | sun`；`ChannelEvent` 值为 `stock_available | monitoring_failed | recovered`；`MonitoringAction`、`ScanAction` 同理为前述 snake_case 小写。`ChannelTest.outcome` 为 `accepted | failed | invalid | not_configured | unknown`，并带 `message: Option<String>`。`ProxyImportResult { added: Vec<String>, duplicates: usize, invalid: Vec<{ line: usize, reason: String }> }`。

## 命令输入类型

`ChannelInput { id, name, providerId, values, subscriptions, bindingId?, targets? }` 仅实现反序列化，不实现 `Debug`/序列化。通知平台为飞书、企业微信、钉钉、微信，均支持扫码绑定。`bindingId` 引用 Core 持有的授权结果，`values` 用于手动配置官方 SDK 凭据；`targets` 为用户选择的通知会话，每项为 `{ id, kind: "user" | "chat", label }`。凭据写入同一个 `monitor.sqlite3` 的 `credentials` 表，使用 `(kind, reference)` 区分渠道和代理凭据。Core 不读取 Keychain 或独立凭据文件。Secret 和凭据引用不进入 `ChannelView`、快照、日志、诊断和配置导出；`configuredFieldKeys` 仅列出已配置字段名。渠道记录 `enabled` 由独立 set_enabled 方法维护。

首次扫码发起时，Core 先登记等待绑定状态，后台空闲配置保留正在准备授权的通知进程；初始请求失败后清除该等待状态。桌面绑定命令为 `begin_channel_binding`、`begin_channel_rebinding`、`channel_binding_status`、`submit_channel_binding_verification`、`cancel_channel_binding`。公开绑定状态包含二维码地址、授权阶段、实际 `connectionStatus`、机器人入口 `botUrl`、钉钉应用名称 `appName`、会话与可获得的 `botName`，不含凭据。钉钉授权后查询本应用元信息，应用名称作为绑定资料返回，机器人名称用于渠道默认命名；未取得接收对象时引导用户发送私信并自动选中已识别的个人对象。授权完成后连接标记按真实连接状态显示，长连接就绪才显示绿勾；钉钉已收到私信却未获得 `senderStaffId` 时明确提示改用机器人所属组织账号或群聊，保留空目标并继续监听有效回调。`detect_notification_channel_groups` 与 `detect_binding_groups` 返回可选择会话；飞书通过官方群列表接口分页发现机器人所在群，其他平台从连接收到的会话事件收集已知群，发现范围由平台接口决定。`ChannelView` 另含 `botName`、`connectionStatus`、`targets`、`selectedTargets` 与 `recipientDeliveries`，分别描述平台机器人名称、连接、已知会话、选择和逐会话投递结果。`ChannelTest.recipients` 返回逐会话测试结果，全部会话被接受才将整体结果记为成功。

飞书扫码保存用户身份及服务域名，验证机器人资料后启动长连接；注册使用明确的权限模板。个人消息与进入机器人会话事件保存 `open_id` 对应的 `chat_id`，已有会话地址优先使用，没有会话地址时使用扫码身份发送。微信账号启动先完成 `notifystart`，就绪后可向绑定账号直接测试；发送携带对应接收人已有的上下文，缺少上下文时由平台判定发送结果。非零 `ret` 或 `errcode` 保留真实拒绝码。账号停止发送 `notifystop`，重开等待旧连接停止完成；扫码保存为渠道时复用在线连接。微信要求手机配对码时进入 `needs_verification`，通过 `submit_channel_binding_verification` 提交并继续原扫码流程。企业微信按操作系统提供扫码平台参数，消息和进入会话事件收集接收对象。钉钉逐接收人检查无效与限流名单，业务拒绝不记为测试成功。

微信首次发送返回 `-2` 且对应会话尚无入站令牌时，显示建立私聊的操作指引。发送请求始终按平台回执判定，连接状态保持真实在线；入站会话信息持久化，正常退出重开后沿用。

桌面添加流程由外部平台按钮固定平台，引导通知页复用同一个弹窗。扫码阶段使用紧凑高度，微信扫码完成后保持紧凑；其他平台扫码完成或进入手动配置后使用配置高度，切回扫码或重新扫码时恢复紧凑高度。高度受当前视口限制，内容较多时内部滚动，操作栏保持可见。弹窗以居中二维码为主，获取期间显示转圈状态，过期后在同一区域显示居中刷新图标。扫码完成后用渠道图标叠加连接状态标记，“提醒内容”选项固定在底部操作栏上方，位于滚动内容之外。名称位于图标下方，输入框宽度随文字调整，编辑按钮紧随名称；命名优先级为自定义、已获得的机器人名称、渠道名与编号。名称下方居中展示个人接收对象，默认私聊名称显示“创建人”，平台提供姓名时保留姓名，个人对象不提供改名操作。微信固定使用创建人私聊，其余平台提供复选框；新建默认选择第一个已识别的个人会话，用户取消后轮询保留选择，编辑保留已保存的选择。群聊选择独立展示，标题注明可选，右侧提供刷新按钮，群聊列表位于操作提示上方，空列表不显示占位内容；微信以外的平台提供折叠的手动填写接收对象入口。手动配置、扫码连接和重新扫码入口位于底栏左侧，取消和保存位于同一行右侧；手动模式的名称输入框位于表单顶部，提醒内容直接展示。新建或更换账号后保存会发送测试通知，成功后按原启用意图启用；名称、订阅与接收对象调整保留连接、启用状态及测试记录。无改动保存直接关闭。群聊由用户显式勾选，微信提供个人会话。企业微信和钉钉通过群内 @机器人消息识别群聊，飞书刷新时调用官方群列表接口。平台未提供群聊名称时，用户可编辑显示名称。机器人链接资料及打开能力保留，配置页默认隐藏机器人跳转按钮。

二维码下方显示当前平台的扫码与手机完成步骤，等待扫码和已扫码阶段保持同一条单行提示，提示后提供刷新文字按钮重新获取二维码。四平台授权完成后继续显示当前初始化步骤，`privateMessageReceived` 表示已收到私信，`privateChatReady` 表示已识别可投递的个人会话；微信依据创建人会话凭据确认，企微与钉钉依据有效私信回调确认，飞书进入机器人单聊即可确认。新扫码保存个人会话前等待初始化，选择群聊可独立完成配置，已有渠道编辑沿用保存的配置。配对期间收到有效私信或群消息后立即尝试回复“收到，配对成功！”，同一绑定会话的每个接收对象确认一次，发送结果通过 `pairingReplies` 呈现，失败或未知结果不自动重发；普通监控连接不进行配对回复。收到私信后提示“已收到私信，配对成功”，确认消息发送未成功时明确提示。群聊区的居中提示按平台分别说明飞书加群后刷新、企业微信在群内发送任意 @消息、钉钉在内部群内发送任意 @消息，微信不显示群聊区；群目录发现与进入聊天事件不冒充收到消息。配对码仅在平台要求时提示。

通知卡片以名称及状态图标标签、所选接收对象两行展示，私聊使用头像符号，群聊使用群组图标；长名称省略，连接、测试及逐对象投递结果通过悬停提示查看。无名称群聊隐藏原始 ID，显示“群聊”；只有平台提供人数时才显示人数。身份去重使用同一账号内的对象类型与平台 ID；钉钉的 `senderId` 与 `senderStaffId` 由平台回调确认关联后合并为员工 ID，同名的不同身份保持独立。

`begin_channel_editing(channelId)` 与 `end_channel_editing(channelId)` 临时保持既有渠道连接，使停用渠道的编辑页也能识别新会话。编辑、测试均复用原有凭据和同一账号连接；编辑关闭后按持久化启用状态恢复，编辑期间不会启用库存投递。多个编辑入口按引用计数释放连接，检测群聊结束不会提前关闭仍在编辑的连接。

通知运行时按实际连接凭据与代理地址判断是否重建连接，字段顺序、机器人名称、消息游标和上下文变化保持当前连接；复用连接时保留运行中的最新上下文。账号停用、移除或真正的凭据、网络路径变化释放旧连接。

修改既有账号的连接凭据时，Core 在配置串行区内先停止旧账号连接，收到确认后清除尚未持久化的旧凭据更新，再保存和应用新凭据。微信正常长轮询超时保留游标并继续轮询；会话令牌按消息序号和时间保留较新的值，令牌与顺序元数据使用同一有界窗口。凭据事件携带这两个字段的完整快照，持久化以最新快照替换，淘汰项随之移除。飞书重复的个人会话映射保持原值，发生变化才发布凭据更新。

企业微信在 SDK 认证成功后判定就绪；认证等待超时关闭旧连接并进入恢复。被其他实例接管时显示 `connection_conflict` 并停止自动争抢，用户停用后重新启用可再次连接。飞书启用官方 SDK 的心跳响应期限，明确认证拒绝转为重新授权；钉钉复用并发令牌刷新，在临近到期前刷新，发送明确遭遇令牌拒绝时刷新后重发一次。网络结果未知的请求保持未知结果。

每个已选择会话在 `notification_routes` 保存独立路由，持久化队列逐路由确认与重试。一个群已接受、另一个群失败时，重试失败群；接收结果未知时不重试。关闭、删除或改选会话会撤销尚未发送的旧请求。平台已接受的动作无法撤回，退出时仍不确定的投递保留未知结果。

未就绪账号的渠道事件保留待发送，在线账号可独立领取。领取后、提交平台前恰好掉线的请求使用内部结果 `deferred` 退回等待；手动测试将该结果显示为本次测试失败。等待连接期间的抢购提醒在恢复和最终提交时核验最新上架、库存与有效期限，已售罄、已下架或过期事件跳过，事件历史保留。正常在线连续放货的事件继续按路由顺序投递，首次上架但库存为零的特殊提醒保持有效。

Core 定期检查通知子进程的响应，存活但无响应的进程会被回收并恢复配置。短期连续退出受重启次数限制，稳定运行后重新计算退出次数；已经提交且接收结果未知的通知保持终态。

有效库存或健康事件提交事务后立即唤醒渠道、系统通知和突出提醒的独立消费者，通知起步与界面快照刷新解耦。不同接收路由并行提交，同一路由保持事件顺序；平台回执在进程状态锁外等待。发送没有统一的固定间隔，平台拒绝与有限重试仍按实际回执处理。系统权限刷新及渠道连接维护的定时任务同时保留事件唤醒入口。

新的监控运行实例取得排他运行锁后，将上次遗留的渠道在途记录结算为接收结果未知，释放路由的顺序占位。新的待发送库存事件继续处理；未确认的旧消息保持未知终态。

`ProxyInput { protocol: ProxyProtocol, host: String, port: u16, username: Option<String>, password: Option<String> }`；代理账号密码与通知凭据一同写入 SQLite 的 `credentials` 表，UI不可读回。旧引用没有 SQLite 凭据时会要求重新添加认证信息，代理请求不会静默退化成无认证请求。协议值为 `http | https | socks5`。代理视图仅含 `displayAddress`、启用状态和 `untested | available | cooldown | auto_disabled | manually_disabled` 状态。`ChannelTest { outcome, message }` 只表示平台业务接受或具体失败，不把 HTTP 发送成功说成用户已读。`ProductScan { id, startId, endId, currentId, checked, found, status, error, results }`；`currentId` 为当前请求中的 Product ID，其他时刻为 `null`。状态为 `running | paused | completed | cancelled | failed`，结果复用 `ProductRecord`，且新发现时 `enabled=false`。

## 平台边界

真实突出提醒提供“我买到了”，关闭该提醒后在主窗口询问“好评／有一些意见／跳过”；同一安装的评价流程关闭后不再自动重复。好评页面提供 GitHub Star 与爱发电“打赏作者”按钮，设置页底部保留同一打赏入口。测试提醒不主动询问评价。设置页的“反馈”直接打开意见表单，关闭该表单不改变购买后评价的状态。反馈提交经桌面命令发送，包含用户填写的内容、应用版本和系统类型；提交前说明这些字段，服务确认持久化后才显示收到，失败保留输入。此流程不自动上传监控数据或账号资料。公开地址由 [`support.json`](../apps/desktop/src/lib/support.json) 定义。

反馈服务使用 [`worker.mjs`](../services/feedback/worker.mjs) 接收 POST `/feedback` 并写入专用 Cloudflare KV；公开入口提供提交，后台内容通过 Cloudflare 控制台查看。部署时参照 [`wrangler.example.json`](../services/feedback/wrangler.example.json) 创建本地配置，使用已有 Wrangler 登录；本地账号配置与凭据不进入仓库。免费额度以 [Workers](https://developers.cloudflare.com/workers/platform/pricing/) 和 [KV](https://developers.cloudflare.com/kv/platform/pricing/) 的平台说明为准。

默认商品目录由 Core 的 `catalog::DEFAULT_PRODUCTS` 定义，覆盖已核实的 GR III／IV 版本、独立套餐及会员卡。快照按编号合并默认目录与用户记录，已保存的名称、监控选择和检查记录优先；读取目录不联网、不入库、不启动监控。默认目录中尚未验证的商品经 `set_product_enabled` 首次启用时复用商品验证与保存流程，失败时保持未启用。前端通过右侧“添加监控／取消”操作提交选择，取消保留目录条目。

前端拥有表单草稿、筛选、导航与外观偏好，命令适配层负责调用 Core 用例、系统操作及平台状态，Core 拥有调度与持久化。外观提供 `light | dark | system`，默认跟随系统并保存于界面本地偏好，切换不调用监控配置或运行命令。系统测试通知通过单一 `test_system_notification` 完成权限查询、必要的申请、权限缓存和发送，返回 `accepted_by_system | permission_denied`，失败保留具体错误。操作结束后，前端读取最新快照；失败后也同步可能已经改变的平台状态。职责和时序见[三层设计](architecture.md)。

Windows 首次通知身份初始化期间，权限读取仅对 `0x80070490` 等待通知平台记录建立，等待上限为两秒；正常权限读取和其他错误立即返回。并发权限查询共用初始化锁，初始化通知抑制弹出并定向清理。清理结果与真实权限结果分别处理。

通知权限读取失败时，监控快照、商品与渠道仍可刷新，平台状态记录 `unavailable` 和具体错误，系统通知暂不领取。自动刷新在已有状态区域显示一处持续错误；普通监控推送保持该错误，成功读取权限后才清除。手动检查以最终读到的权限状态为准，持续失败保留具体错误，界面不重复展示同一错误。窗口重新获得焦点时沿用这套状态规则，不增加横幅延时或通用重试。

Tauri 保留 `MonitorApp` 状态、serde 快照广播、系统通知、托盘菜单、关窗后台、登录启动和打开外部链接。桌面系统状态通过平台扩展呈现；Core 将通知与代理凭据保存在应用 SQLite 数据库中，持有通知子进程、连接配置和逐会话投递队列。独立通知模块以内置 Bun 加自包含脚本调用官方 SDK，处理扫码授权、长连接、平台会话和发送结果，通过 NDJSON 与 Core 通信。关窗后台与监控暂停保留启用渠道连接；正式退出等待 Core 结束并回收子进程。Apple Silicon 与 Windows x64 构建自带对应平台模块，用户无需安装 Node 或 Bun；CLI 通过 `RM_NOTIFICATION_RUNTIME_PATH` 指定运行时位置，通过 `RM_NOTIFICATION_RUNTIME_SCRIPT` 指定脚本位置，默认使用运行时同目录的 notification-runtime.mjs。托盘的暂停/恢复直接调用共享 `monitoring_action`。

macOS 的权限查询、测试通知与监控通知统一使用 `UNUserNotificationCenter`。命令层等待提交回调并返回系统错误，应用前台展示由通知 delegate 负责；界面仅在权限尚未允许时显示授权状态。

Windows 桌面版与 macOS 共用界面和业务用例。初始窗口外框超过当前显示器工作区时使用系统最大化，首次引导及页面操作区保持可见；工作区扣除任务栏，并按实际像素比较。登录后启动使用当前用户的登录启动项，保存已安装应用的完整路径，启用与停用后的状态返回界面。目录由资源管理器打开，HTTP／HTTPS 链接交给默认浏览器；系统通知使用与安装器一致的应用标识，并在查询权限或投递前注册当前用户的通知应用信息与协议激活入口。首次权限查询缺少系统通知记录时，无横幅初始化后清除初始化消息，再读取真实权限；点击通知通过单实例入口显示主窗口，删除开始菜单快捷方式后仍可查询权限与投递，Windows 禁用或策略状态按真实权限返回。应用关闭窗口后保留系统托盘入口，退出时等待共享监控任务结束。

Linux CLI 保留交互式设置、状态查看、商品/渠道/代理操作、扫描、测试、前台运行控制和 systemd unit 输出，直接调用相同 `MonitorApp`。删除 Unix socket/JSON-RPC 服务及其 framing、客户端管理和第二套命令协议；systemd 由 CLI 输出的 unit 管理进程，不建立第二个业务后台服务。

两端使用同一配置、扫描、库存、渠道发送与运行意愿。运行中调整检查节奏或提醒阈值保留当前请求、连接池和健康计时，后续请求使用新参数；保存计划同步更新共享请求入口，计划外不再提交新的监控请求；已开始的请求可以保存有效结果，进入有效时段后继续。网络设置或实际出口变化重建网络任务。暂停、停止、重新开始及配置导入结束旧商品轮次，立即恢复也遵守该轮次边界。待投递通知绑定发生时的商品轮次，已结束轮次的消息跳过；正常退出保留轮次和待发送记录。通知在确认未被接收且可重试时执行有限重试，接收结果未知时保留诊断。容量、发送期限和结果保留范围由 `storage/notification_outbox.rs` 定义；队列达到上限时记录明确失败结果。凭据不进入快照、日志、诊断或导出。
