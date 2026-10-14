import { desktopApi, type DesktopSnapshot } from "$lib/desktop-api";

// 新增升级项按展示顺序追加；待确认状态由 Core 持久化，跨版本升级取全部未完成项。
export const upgradeItems = [
  { id: "configuration", title: "新功能：配置变更提醒", choice: "启用配置变更提醒",
    pending: (snapshot: DesktopSnapshot) => snapshot.configurationChangeIntroPending && snapshot.channels.length > 0,
    save: (enabled: boolean) => desktopApi.completeConfigurationChangeIntro(enabled) },
  { id: "usage", title: "新功能：使用统计", choice: "同意分享使用统计",
    pending: (snapshot: DesktopSnapshot) => snapshot.usageStatisticsIntroPending,
    save: (enabled: boolean) => desktopApi.setUsageStatisticsEnabled(enabled) },
] as const;

export function pendingUpgradeItems(snapshot: DesktopSnapshot) {
  return snapshot.setupCompleted ? upgradeItems.filter((item) => item.pending(snapshot)) : [];
}
