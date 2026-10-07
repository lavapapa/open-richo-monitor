import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, expect, it } from "vitest";
import ProviderIcon from "./ProviderIcon.svelte";

afterEach(cleanup);

it("微信使用官方双气泡应用图标", () => {
  render(ProviderIcon, { providerId: "weixin", name: "微信" });
  expect(screen.getByAltText("微信").getAttribute("src")).toBe("/providers/weixin.png");
});
