import * as Alchemy from "alchemy"
import * as Cloudflare from "@alchemy.run/cloudflare"
import { Stack } from "alchemy/Stack"
import * as Effect from "effect/Effect"

// The site is a Worker with static assets: `bun run build` writes dist, and
// Cloudflare's asset layer serves it. This stack only deploys to prod, so any
// other stage fails before it can touch Cloudflare.
export default Alchemy.Stack(
  "tinybrowser",
  {
    providers: Cloudflare.providers(),
    state: Cloudflare.state(),
  },
  Effect.gen(function* () {
    const stack = yield* Stack
    if (stack.stage !== "prod") {
      return yield* Effect.die(
        new Error(`tinybrowser only deploys to prod; got "${stack.stage}". Pass --stage prod.`),
      )
    }

    const site = yield* Cloudflare.Website.StaticSite("Website", {
      command: "bun run build",
      outdir: "dist",
      main: "./src/worker.ts",
      assets: {
        notFoundHandling: "404-page",
        runWorkerFirst: ["/api/*"],
      },
      domain: "tinybrowser.ericc.ch",
      workersDev: false,
    })

    return { url: site.url }
  }),
)
