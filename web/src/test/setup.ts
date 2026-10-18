import "@testing-library/jest-dom/vitest";
import { afterAll, afterEach, beforeAll } from "vitest";

import { resetConnectivity } from "../lib/connectivity";
import { server } from "./msw";

beforeAll(() => {
  server.listen({ onUnhandledFrame: "error" });
});

afterEach(() => {
  server.resetHandlers();
  resetConnectivity();
});

afterAll(() => {
  server.close();
});
