import test from "node:test";
import assert from "node:assert/strict";

import { double } from "./math.mjs";

test("double multiplies by two", () => {
  assert.equal(double(3), 6);
});
