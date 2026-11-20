import * as Alchemy from "alchemy"
import { AlchemyContext } from "alchemy/AlchemyContext"
import * as Cloudflare from "@alchemy.run/cloudflare"
import { Stack } from "alchemy/Stack"
import * as State from "alchemy/State"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The site is an Astro project deployed as a Cloudflare Worker with static
// assets (`Cloudflare.Website.Astro`): pages opt into build-time prerendering
// and `/api/*` endpoints execute inside the Worker. Deploys only target
// production (using the Cloudflare state store); `alchemy dev` uses local state
// under `.alchemy/state` and runs Astro's dev server with SSR/API endpoints in
// workerd on localhost:4321.
export default Alchemy.Stack(
  "tinybrowser",
  {
    providers: Cloudflare.providers(),
    state: Layer.unwrap(
      Effect.map(AlchemyContext, (ctx) => (ctx.dev ? State.localState() : Cloudflare.state())),
    ),
  },
  Effect.gen(function* () {
    const ctx = yield* AlchemyContext
    const stack = yield* Stack
    if (!ctx.dev && stack.stage !== "production") {
      return yield* Effect.die(
        new Error(
          `tinybrowser only deploys to production; got "${stack.stage}". Pass --stage production.`,
        ),
      )
    }

    const site = yield* Cloudflare.Website.Astro("Website", {
      sessionKVBindingName: false,
      prerenderEnvironment: "node",
      assets: {
        notFoundHandling: "404-page",
        runWorkerFirst: ["/api/*"],
      },
      domain: "tinybrowser.ericc.ch",
      workersDev: false,
      dev: { port: 4321 },
    })

    return { url: site.url }
  }),
)
