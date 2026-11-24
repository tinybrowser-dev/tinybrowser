import { defineConfig } from "astro/config"

// The WPT dashboard is a route, not a base path: the home page is the same
// site, and the raw data lives under /wpt/data so a run can be fetched with
// curl. No client framework: every page is rendered once at build time and the
// two interactive pieces are a few lines of inline script.
export default defineConfig({
  site: "https://tinybrowser.ericc.ch/",
  compressHTML: true,
  // Dev is reached from a phone through `tailscale serve`, which proxies to
  // this port and forwards the tailnet hostname as the Host header.
  server: {
    allowedHosts: [".barbel-fish.ts.net"],
  },
})
