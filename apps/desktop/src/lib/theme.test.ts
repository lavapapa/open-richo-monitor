import { describe, expect, it } from "vitest";
import themeCss from "./theme.css?raw";
import startup from "../../static/theme-init.js?raw";

function palette(selector: string): Record<string, string> {
  const start = themeCss.indexOf(`${selector} {`);
  const body = themeCss.slice(start, themeCss.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--([\w-]+): (#[\da-f]{6});/g)].map((match) => [match[1], match[2]]));
}

function luminance(hex: string) {
  const channels = hex.slice(1).match(/../g)!.map((channel) => parseInt(channel, 16) / 255);
  const linear = channels.map((value) => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4);
  return linear[0] * .2126 + linear[1] * .7152 + linear[2] * .0722;
}

function contrast(a: string, b: string) {
  const pair = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (pair[0] + .05) / (pair[1] + .05);
}

describe("主题", () => {
  it.each(["light", "dark"])("%s 主题的正文、辅助文案和状态满足文字对比度", (mode) => {
    const colors = palette(mode === "light" ? ":root" : ':root[data-theme="dark"]');
    for (const background of ["canvas", "surface", "subtle", "control", "table-heading", "sidebar"]) {
      for (const foreground of ["foreground", "muted"]) {
        expect(contrast(colors[foreground], colors[background]), `${foreground}/${background}`).toBeGreaterThanOrEqual(4.5);
      }
    }
    for (const [foreground, background] of [["accent-text", "accent-soft"], ["positive", "surface"], ["danger", "danger-soft"], ["success-text", "success-bg"]]) {
      expect(contrast(colors[foreground], colors[background]), `${foreground}/${background}`).toBeGreaterThanOrEqual(4.5);
    }
    expect(contrast("#ffffff", colors["primary-bg"])).toBeGreaterThanOrEqual(4.5);
    expect(contrast("#ffffff", colors["accent-hover"])).toBeGreaterThanOrEqual(4.5);
    expect(contrast(colors.accent, colors.control)).toBeGreaterThanOrEqual(3);
  });

  it("系统模式具有实时媒体查询，手动主题保持覆盖", () => {
    expect(themeCss).toContain("@media (prefers-color-scheme: dark)");
    expect(themeCss).toContain(':root:not([data-theme="light"]):not([data-theme="dark"])');
    expect(themeCss).toContain(':root[data-theme="dark"]');
  });

  it.each(["light", "dark", "system", null])("首屏脚本在界面挂载前恢复 %s 外观", (value) => {
    localStorage.clear();
    if (value) localStorage.setItem("rm.theme", value);
    new Function(startup)();
    expect(document.documentElement.dataset.theme).toBe(value ?? "system");
  });
});
