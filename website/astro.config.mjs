import { defineConfig } from "astro/config"
import solid from "@solidjs/vite-plugin"

// The WPT dashboard is a route, not a base path: the home page is the same
// site, and the raw data lives under /wpt/data so a run can be fetched with
// curl. Only `/demo` mounts a client-side Solid 2.0 app.
export default defineConfig({
  site: "https://tinybrowser.ericc.ch/",
  compressHTML: true,
  vite: {
    plugins: [solid()],
  },
  // Dev is reached from a phone through `tailscale serve`, which proxies to
  // this port and forwards the tailnet hostname as the Host header.
  server: {
    allowedHosts: [".barbel-fish.ts.net"],
  },
})
