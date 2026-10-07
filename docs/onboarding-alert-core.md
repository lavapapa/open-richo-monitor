# 引导与突出提醒核心契约

本契约约定首次引导选品、上架事件与突出提醒的业务边界。桌面和命令行共用 Core 的商品目录、观察归并与通知文案，桌面平台负责展示屏幕覆盖图层。

## 一、选品

`MonitorApp::set_onboarding_products(&[u64])` 在一次事务内保存完整监控选择。选择项来自默认目录或已保存的用户目录；默认目录可直接启用，已有商品名称、验证记录、元数据和突出提醒选择保持保存值。空选择及无效 ID 使整个选品事务回滚。取消监控会结束对应商品轮次，保持选择的商品继续原轮次。

移除默认目录商品会跨重启隐藏该条目；通过显式选品、手动添加或后续有效扫描可重新加入。删除标记与当前扫描轮次共同判定，避免在途扫描覆盖刚完成的删除。

`MonitorApp::complete_setup` 要求至少一个已启用商品；成功后保存引导完成状态，并按照自动监控偏好保存运行意图。引导完成页独立提供“登录后启动”和“启动后自动开启监控”，后者默认开启；完成页不显示步骤编号。应用启动规则见 [应用契约](app-contract.md#core-服务)。入口和实际判定见 [app.rs](../crates/core/src/app.rs) 的 `set_onboarding_products`、`complete_setup` 和 `setup_ready`；事务实现见 [storage.rs](../crates/core/src/storage.rs) 的 `Storage::set_onboarding_products`。

## 二、事件

上架以 `isShow=1` 判断。首次观察到上架、下架转上架、无货转有货都触发既有 `stock_available` 订阅。首次上架即使库存为零也生成事件；同一观察同时上架和有货时生成一个事件，重复观察保持去重。列表方案与逐商品方案共用 [reducer.rs](../crates/core/src/availability/reducer.rs) 的 `ObservationReducer::prepare`。

商品持续上架且有货时，真实库存比上一条有效观察增加会生成 `stock_increased` 提醒，并交付系统通知、已开启且订阅 `stock_available` 的渠道，以及已开启的突出提醒。库存增量历史复用 `check_runs`，提醒复用事件序列与 outbox。相同库存、下降、缺失库存及迟到观察保持原有判定。

商品的 `prominentAlert` 由 `MonitorApp::set_product_prominent_alert` 保存。导出和导入配置包含这个字段；移除商品及恢复默认会清除该商品的突出设置。通知文案由 [app.rs](../crates/core/src/app.rs) 的 `notification_text` 统一构造：首次上架和再次上架使用“上架”，保持上架时从无货到有货及正库存增加使用“补货”，已知前值时展示库存变化；突出商品保留“立即抢购”前缀。系统通知与渠道通知使用同一完整正文，通知、引导和历史中的用户可见名称保持一致。

库存事件在提交事务中保存此前上架状态与可获得的库存；监控异常事件保存发生时的原因、连续失败次数和有效监控失败时长。事件和待投递记录各自持有这些信息，后续补货、恢复、历史清理与应用重开保持先前通知的事件内容。失败次数按同商品、同轮次的有效检查累计，成功检查清零；暂停、计划外和退出期间不增加次数或有效失败时长。持久化实现见 [storage.rs](../crates/core/src/storage.rs) 的 `EventNotificationDetails`、`commit_observation`、`commit_health_transition` 及 [runtime_storage.rs](../crates/core/src/storage/runtime_storage.rs) 的 `record_monitoring_runtime`。

`MonitorApp::claim_prominent_alert` 返回可选 `ProminentAlert`，其字段及 camelCase 序列化以 [app.rs](../crates/core/src/app.rs) 的同名结构为准。远端主图地址和已匹配的本地缓存路径都可用于展示；价格保持源接口的十进制文本。

突出提醒使用 outbox 的 `__prominent__` 专用渠道。同一商品、同一轮次的待展示提醒合并为最新事件，保持首次入队位置；事件历史、系统通知和群通知保持各自记录。领取事务核验商品仍在目录、仍启用、仍勾选突出提醒、商品轮次匹配且最新观察仍为上架；库存为正的事件还要求最新库存保持为正，零库存上架事件仍可交付。取消监控、切换方案、移除商品产生的旧任务会被清理。领取保留有效任务，平台成功展示后调用 `MonitorApp::acknowledge_prominent_alert(event_id)` 确认并消费该事件；展示失败时保留任务，下次领取重新验证有效性。平台按单实例串行展示，成功确认后的事件结束交付。专用消费者独立于系统通知开关及渠道设置，普通渠道与系统消费者分别处理各自任务。容量和短时有效期见 [notification_outbox.rs](../crates/core/src/storage/notification_outbox.rs) 的 `PENDING_PER_CHANNEL`、`TOTAL_PENDING` 和 `PROMINENT_MAX_AGE_MS`；入队与领取时清除过期及失效任务。有效的容量失败项在领取时一次汇总并消费，返回错误供桌面提示，后续领取继续处理有效提醒。

桌面首次开启商品铃铛时先显示测试确认。取消保持关闭，测试成功展示该商品提醒后保存本机已测试状态并开启铃铛；后续开启免确认。通用测试入口的成功展示也记录已测试状态，展示失败保留确认界面及关闭状态。

突出提醒窗口在应用启动时提前加载，关闭后隐藏并复用。监控事务提交后立即唤醒专用消费者，触发路径不等待快照节流或固定轮询。后端直接把已领取的商品数据交给已加载的页面；页面提交当前事件的内容后才显示并确认交付。窗口显示优先使用既有本地图片缓存，缓存图片解码后显示；远端图片保持异步加载，失败的展示保持队列待交付状态。连续提醒必须更新商品、库存、价格和关闭状态，隐藏的预加载窗口不参与托盘及重复启动的提醒焦点判定。

Windows 提醒显示期间由系统接收 Esc、空格、回车关闭键，关闭、显示失败和退出均释放快捷键。完整覆盖图层与关闭操作独立于原应用是否让出前台焦点，显示链不连接外部程序的输入队列。验收须从其他全屏应用后台触发，逐键关闭，并检查隐藏后按键已返回原应用。

突出提醒遮罩与提醒卡片始终完全不透明。Esc、空格和回车均可关闭。性能验收分别记录触发、窗口可见、当前商品内容可操作及实际屏幕截图时刻；使用同一台设备、同一种触发方式比较优化前后，工具查询耗时按可观测上界说明。

桌面重新打开、重复启动和托盘打开入口共用焦点处理：已有提醒时聚焦同一提醒面板，保持当前事件；关闭后沿用主窗口入口。

## 三、验证

核心测试验证零库存首次上架、再次上架、后续补货、重复检查、通知订阅隔离、系统/渠道/突出提醒并行交付、持久队列重开、突出提醒确认不影响普通通知、展示失败后的重新领取与有效性复核、取消与方案切换、期限与容量、原子引导选品、元数据保留和配置生命周期。端到端测试 [list_monitoring.rs](../crates/core/tests/list_monitoring.rs) 的 `both_modes_emit_one_prominent_alert_for_a_zero_stock_listing_without_notifications` 验证两种监控方案连续检查时，在关闭系统通知且没有渠道配置的条件下交付一次零库存突出提醒。

`wait_for_prominent_alert` 使用事务提交后保留的唤醒许可；桌面启动先领取持久队列，关闭当前提醒后调用 `wake_prominent_alerts` 继续领取，退出也唤醒等待者。显示序号随页面数据回传，在原生主线程显示时核验；过期与重复回调无法显示旧提醒或完成新交付。

执行 `cargo test --manifest-path crates/core/Cargo.toml` 验证核心契约，执行 `cargo check --manifest-path crates/cli/Cargo.toml` 验证命令行编译。桌面透明覆盖、平台置顶和提醒关闭行为由实际 App 验收覆盖。

核心交付保证事件成立条件、商品选择、有效性复核和展示确认后的消费；平台展示以 `ProminentAlert` 的结构和领取结果为入口。
