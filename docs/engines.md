# Engines

- Chromium
  - Search: https://source.chromium.org/chromium/chromium/src
  - Single file: https://raw.githubusercontent.com/chromium/chromium/<rev>/path
- Firefox
  - Search: https://searchfox.org/firefox-main/source/
  - Single file: https://raw.githubusercontent.com/mozilla-firefox/firefox/<rev>/path
- WebKit
  - Search: https://searchfox.org/wubkat/source/
  - Single file: https://raw.githubusercontent.com/WebKit/WebKit/<rev>/path
- Ladybird
  - Search: https://github.com/LadybirdBrowser/ladybird
  - Single file: https://raw.githubusercontent.com/LadybirdBrowser/ladybird/<rev>/Libraries/LibWeb/path
  - Browse: https://github.com/LadybirdBrowser/ladybird/tree/master/Libraries/LibWeb

rev is a commit hash or branch (e.g. main, master for Ladybird). Old
searchfox.org/mozilla-central URLs redirect to firefox-main.

Ladybird keeps the web platform in `Libraries/LibWeb`, with the worker
interfaces in `HTML/` (`Worker.cpp`, `WorkerGlobalScope.cpp`,
`DedicatedWorkerGlobalScope.cpp`, `WorkerAgentParent.cpp`) rather than in a
`workers/` directory. GitHub's code search needs a signed-in session in the
browser; the CLI form is
`gh api --method GET search/code -f q='repo:LadybirdBrowser/ladybird Worker'`.
