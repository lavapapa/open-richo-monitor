import { describe, expect, it } from "vitest";
import macConfig from "../../src-tauri/tauri.macos.conf.json";
import sharedConfig from "../../src-tauri/tauri.conf.json";

describe("macOS 全尺寸内容区", () => {
  it("主窗口保留系统按钮、覆盖式标题栏并隐藏重复标题", () => {
    expect(macConfig.app.windows[0]).toMatchObject({
      label: "main", decorations: true, titleBarStyle: "Overlay", hiddenTitle: true,
    });
    expect(sharedConfig.app.windows[0]).not.toHaveProperty("titleBarStyle");
  });
});
