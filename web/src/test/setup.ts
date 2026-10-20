import "@testing-library/jest-dom/vitest";
import { afterAll, afterEach, beforeAll, beforeEach } from "vitest";

import { resetConnectivity } from "../lib/connectivity";
import { mockProjects, mockRepos, server } from "./msw";

beforeAll(() => {
  server.listen({ onUnhandledFrame: "error" });
});

beforeEach(() => {
  // Every console surface reads the project rollup, so it must resolve even
  // in tests that only exercise another endpoint. A test-specific handler
  // registered later takes precedence.
  server.use(mockProjects());
  // The registry backs the project page's edit/register actions.
  server.use(mockRepos());
});

afterEach(() => {
  server.resetHandlers();
  resetConnectivity();
});

afterAll(() => {
  server.close();
});
