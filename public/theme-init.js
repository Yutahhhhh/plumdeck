// Applies the saved light/dark theme class before the first paint, so people
// who use the dark theme do not see a white flash while the app (and its
// startup screen) loads. Mirrors ThemeProvider in src/main.tsx: storage key
// "plumdeck-ui-theme", and "system" (the default) follows the OS setting.
// Loaded as a plain, render-blocking script from public/ so the CSP's
// script-src 'self' allows it without an inline hash.
(function () {
  var theme = "system";
  try {
    theme = window.localStorage.getItem("plumdeck-ui-theme") || "system";
  } catch (error) {
    // Storage can be unavailable; fall back to the OS setting.
  }
  var dark = theme === "dark" || (theme !== "light" && !!window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
  var root = document.documentElement;
  root.classList.remove("light", "dark");
  root.classList.add(dark ? "dark" : "light");
})();
