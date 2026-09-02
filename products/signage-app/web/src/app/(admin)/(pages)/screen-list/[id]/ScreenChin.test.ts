import { describe, expect, it } from "vitest";
import { shadeOpacityAt, shapeAt } from "./ScreenChin";

describe("ScreenChin dependent geometry", () => {
  it("starts both entrance curves from the closed chin", () => {
    expect(shapeAt(60)).toEqual({ crownY: 0, filletY: 0 });
  });

  it("shares a short entrance between the crown and fillets", () => {
    const shape = shapeAt(86);
    expect(shape.crownY).toBeCloseTo(15.36, 2);
    expect(shape.filletY).toBeCloseTo(10.64, 2);
    expect(shape.crownY + shape.filletY).toBeCloseTo(26);
  });

  it("reaches and holds the full design geometry", () => {
    expect(shapeAt(104)).toEqual({ crownY: 26, filletY: 18 });
    expect(shapeAt(180)).toEqual({ crownY: 26, filletY: 18 });
  });

  it("derives the shadow from the same exposed height", () => {
    expect(shadeOpacityAt(60)).toBe(0);
    expect(shadeOpacityAt(82)).toBe(0.5);
    expect(shadeOpacityAt(104)).toBe(1);
  });
});
