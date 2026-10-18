// Runs before the app bundle to pick the theme without a flash of the wrong
// palette. This must stay an external classic script: the production CSP is
// `script-src 'self'` with no inline exceptions.
//
// The storage key is shared with the theme toggle; keep it stable.
(function () {
  "use strict";

  var STORAGE_KEY = "nuwa-theme";
  var theme;

  try {
    theme = window.localStorage.getItem(STORAGE_KEY);
  } catch {
    // localStorage can throw (privacy mode / disabled storage); fall back to
    // the operating-system preference below.
    theme = null;
  }

  if (theme !== "dark" && theme !== "light") {
    theme = window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }

  document.documentElement.setAttribute("data-theme", theme);
})();
