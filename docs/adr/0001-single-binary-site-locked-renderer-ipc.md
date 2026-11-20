# Single binary with site-locked, value-only renderer IPC

Tinybrowser ships as one executable that runs the browser coordinator, CDP/WebDriver servers, and child `--renderer` processes (`main → {browser, cdp, webdriver}`, `browser → {dial, net, renderer}`). Each renderer process is locked to a registrable site (`cookies::site`) and talks to the browser over a length-prefixed Postcard channel carrying typed values (`Frame`, `Command`, `ServiceCall`) rather than JSON or shared memory, with the browser process acting as the sole trust and I/O boundary.
