import { describe, expect, it } from "vitest";

import { canEditPage, isProtectedTitle } from "./protected";

describe("protected pages", () => {
  it("recognizes Policy: and Wiki: titles however they are spelled", () => {
    for (const title of [
      "Wiki: Schema",
      "Policy: P2P",
      "wiki: schema",
      "pOlIcY:x",
      "Wiki : Schema",
      "  Wiki: Schema",
      "Ｗｉｋｉ： Schema",
      "WiKi: Schema",
      "​Wiki: Schema",
      "W‍iki: Schema",
      "Wiki﻿: Schema",
    ]) {
      expect(isProtectedTitle(title), title).toBe(true);
    }
    for (const title of [
      "Wiki",
      "Wikipedia: notes",
      "Policies: travel",
      "Topic: Wiki: Schema",
      "W iki: Schema",
      "",
      // Look-alike letters are other titles, as on the server.
      "Wіki: Schema",
      "Wıki: Schema",
      "Wİki: Schema",
    ]) {
      expect(isProtectedTitle(title), title).toBe(false);
    }
  });

  it("lets only the teamspace owner edit them", () => {
    expect(canEditPage("admin", "Wiki: Index")).toBe(true);
    expect(canEditPage("writer", "Wiki: Index")).toBe(false);
    expect(canEditPage("writer", "Notes")).toBe(true);
    expect(canEditPage("writer", undefined)).toBe(true);
    expect(canEditPage("reader", "Notes")).toBe(false);
    expect(canEditPage(undefined, "Notes")).toBe(false);
  });
});
