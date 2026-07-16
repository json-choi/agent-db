import { describe, expect, it } from "vitest";
import { isNearBottom } from "./autoScroll";

describe("isNearBottom", () => {
  it("is true when scrolled to the very bottom", () => {
    expect(isNearBottom({ scrollHeight: 1000, scrollTop: 400, clientHeight: 600 }, 80)).toBe(
      true,
    );
  });

  it("is true within the threshold", () => {
    expect(isNearBottom({ scrollHeight: 1080, scrollTop: 400, clientHeight: 600 }, 80)).toBe(
      true,
    );
  });

  it("is false once scrolled up past the threshold", () => {
    expect(isNearBottom({ scrollHeight: 2000, scrollTop: 900, clientHeight: 600 }, 80)).toBe(
      false,
    );
  });
});
