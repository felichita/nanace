import { describe, expect, it } from "vitest";

import { allowedModes } from "./p2p";

describe("P2P accept modes", () => {
  it("offers nothing above what the team policy allows", () => {
    expect(allowedModes("ask")).toEqual(["off", "ask"]);
    expect(allowedModes("team")).toEqual(["off", "ask", "trusted", "team"]);
    expect(allowedModes("off")).toEqual(["off"]);
    // No policy: nothing can be accepted.
    expect(allowedModes(undefined)).toEqual(["off"]);
  });
});
