# Website

The public site has the home page and the WPT dashboard at `/wpt`. Astro, plain
CSS, no client framework. Every page is rendered at build time.

    bun install
    WPT_DATA_DIR=../data bun run dev

`WPT_DATA_DIR` points at a checkout of
[`tinybrowser-dev/tinybrowser-wpt-results`](https://github.com/tinybrowser-dev/tinybrowser-wpt-results).
Without it the dashboard renders its empty state, which is what the first
deploy looks like.

## Checks

    bun run check

`check` runs contrast, typecheck, lint, format, and build. CI runs the same
commands, then Alchemy deploys `dist/`.

| script      | what it does                                         |
| ----------- | ---------------------------------------------------- |
| `contrast`  | validates the token pairs in `src/styles/themes.css` |
| `typecheck` | `astro sync` + `tsc --noEmit --runExternalCode`      |
| `lint`      | oxlint                                               |
| `format`    | oxfmt in check mode. `format:fix` writes             |
| `build`     | copies the record, then `astro build`                |

`@/` is the only path alias. It points at `src/`. Everything else is a
relative import.

## The data the page reads

The dashboard is a static snapshot. `scripts/sync-data.ts` copies the record
from the results repo into `public/wpt/data` before `astro build`, so the files
the page reads are also the files a script can fetch:

| file                | contents                                               |
| ------------------- | ------------------------------------------------------ |
| `latest.json`       | the newest run: totals, per-folder counters, pin, SHA  |
| `index.json`        | one entry per run: date, SHA, totals                   |
| `regressions.json`  | failure-list diff against the previous run             |
| `failures.jsonl.gz` | every failing test and subtest, one JSON object a line |

`latest.json`, `index.json`, and `regressions.json` come from
`uv run tools/wpt/cli.py publish` in the `WPT` workflow. There is no API
version. The shapes above are what this site consumes, and if a shape changes,
the page changes in the same commit.

A run produced by a different WPT revision is not comparable, so
`regressions.json` reports `comparable: false` and an empty diff across a pin
bump. New `TIMEOUT`s are listed separately from new failures. They flip with
runner load.

## Deploy

`.github/workflows/website.yml` deploys with `bunx alchemy deploy --stage prod`
on two triggers: a push that touches `website/**`, and the completion of a `WPT`
run (whose publish step may have moved the data). The stack in
`alchemy.run.ts` is a Cloudflare Worker with static assets, served at
`tinybrowser.ericc.ch`.

The workflow needs `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` as repo
secrets.

## Theme

The palette and type scale follow `https://ericc.ch/DESIGN.md`: monochrome,
`light-dark()` token pairs in `src/styles/themes.css`, type carrying the
hierarchy, text-only controls. `src/styles/base.css` is the reset and reading
rhythm. `src/styles/ui.css` has the page's own classes (`.card`, `.stats`,
`.bar`, `.diff-counts`). There is no Tailwind and no client CSS framework.
