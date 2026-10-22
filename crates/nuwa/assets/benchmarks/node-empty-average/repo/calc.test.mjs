import test from "node:test";
import assert from "node:assert/strict";

import { average } from "./calc.mjs";

test("average of numbers", () => {
  assert.equal(average([2, 4, 6]), 4);
});

test("average of an empty list is zero", () => {
  assert.equal(average([]), 0);
});
