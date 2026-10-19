import { describe, expect, it } from "vitest";
import { desktopShortcut, nextSidebarIndex, allowsContextMenu } from "./desktop-interactions";

describe("桌面输入约定", () => {
  it("Mac 使用 Command，其他系统使用 Control", () => {
    expect(desktopShortcut(new KeyboardEvent("keydown", { key: ",", metaKey: true }), true)).toBe("settings");
    expect(desktopShortcut(new KeyboardEvent("keydown", { key: "f", metaKey: true }), true)).toBe("find-products");
    expect(desktopShortcut(new KeyboardEvent("keydown", { key: "5", metaKey: true }), true)).toBe("products");
    expect(desktopShortcut(new KeyboardEvent("keydown", { key: ",", ctrlKey: true }), false)).toBe("settings");
    expect(desktopShortcut(new KeyboardEvent("keydown", { key: ",", ctrlKey: true }), true)).toBeNull();
  });

  it("输入法、重复按键及其他修饰组合保留原有用途", () => {
    for (const options of [{ isComposing: true }, { repeat: true }, { shiftKey: true }, { altKey: true }]) {
      expect(desktopShortcut(new KeyboardEvent("keydown", { key: "f", metaKey: true, ...options }), true)).toBeNull();
    }
  });

  it("侧栏方向键和首尾键保持列表边界", () => {
    expect(nextSidebarIndex("ArrowDown", 0, 6)).toBe(1);
    expect(nextSidebarIndex("ArrowUp", 0, 6)).toBe(0);
    expect(nextSidebarIndex("ArrowDown", 5, 6)).toBe(5);
    expect(nextSidebarIndex("Home", 3, 6)).toBe(0);
    expect(nextSidebarIndex("End", 3, 6)).toBe(5);
    expect(nextSidebarIndex("Tab", 3, 6)).toBeNull();
  });

  it("输入区和可复制内容保留系统右键菜单，界面控件去掉网页菜单", () => {
    const button = document.createElement("button");
    const input = document.createElement("input");
    const log = document.createElement("pre");
    expect(allowsContextMenu(button)).toBe(false);
    expect(allowsContextMenu(input)).toBe(true);
    expect(allowsContextMenu(log)).toBe(true);
    const message = document.createElement("td");
    message.setAttribute("data-selectable", "");
    expect(allowsContextMenu(message)).toBe(true);
  });
});
