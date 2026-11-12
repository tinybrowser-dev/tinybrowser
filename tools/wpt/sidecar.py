#!/usr/bin/env python3
"""Run wptserve for the in-process WPT harness.

The harness answers every request from memory except two kinds: a `.py` path,
which is a program rather than a file, and a path with no file behind it, which
wptserve answers 404. Both need the real server, so `cli.py run in-process`
starts this process and forwards those requests here.

The server binds a loopback port and reports it on stdout (`PORT <n>`). Ports
inside the test config stay at their standard values: the page requests
`web-platform.test:8000`, so `{{ports[http][0]}}` and `request.url_parts` must
agree with it even though the socket is somewhere else.

It exits when its stdin closes, so it cannot outlive the harness.
"""

import argparse
import logging
import os
import signal
import sys
import uuid

# The checkout is not on the path when this file is run from the repository
# root; `tools.localpaths` then puts `tools/`, `wptserve`, and the vendored
# dependencies on it.
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir, os.pardir))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--wpt-root", required=True)
    args = parser.parse_args()

    wpt_root = os.path.abspath(args.wpt_root)
    sys.path.insert(0, wpt_root)

    from tools.serve import serve
    from wptserve import stash
    from wptserve.server import Http1WebTestRequestHandler, WebTestHttpd

    class SchemeAwareHandler(Http1WebTestRequestHandler):
        """Restores the browser-visible scheme of the forwarded request.

        The harness serves files itself and forwards only handler routes here
        over a plain loopback connection, so wptserve would see `http` for an
        `https` test and build a keep-scheme redirect on the wrong scheme. The
        harness names the scheme it saw; this puts it back before any handler
        reads `request.url`.
        """

        def parse_request(self):
            parsed = super().parse_request()
            if parsed:
                scheme = self.headers.get("X-WPT-Scheme")
                if scheme in ("http", "https"):
                    self.scheme = scheme
            return parsed

    logging.basicConfig(stream=sys.stderr, level=logging.WARNING, format="%(message)s")
    logger = logging.getLogger("wpt-sidecar")

    config = serve.ConfigBuilder(logger, ws_extra=None)
    config.doc_root = wpt_root
    config.check_subdomains = False
    config.server_host = "127.0.0.1"
    config.bind_address = False
    # wptrunner also sets this; the route builder reads it.
    config.inject_script = None
    # `TestEnvironment.build_config`'s table, so a handler and the harness's
    # template substitution agree on every `{{ports[...]}}` value.
    config.ports = {
        "http": [8000, 8001],
        "http-local": [8002],
        "http-public": [8003],
        "https": [8443, 8444],
        "https-local": [8445],
        "https-public": [8446],
        "ws": [8888],
        "wss": [8889],
        "h2": [9000],
        "webtransport-h3": [11000],
        "dns": [8053],
    }

    # The handlers' stash lives in a manager process; `StashServer` also
    # exports WPT_STASH_CONFIG, which `request.server.stash` reads.
    with stash.StashServer(authkey=str(uuid.uuid4())):
        with config as active:
            routes = serve.get_route_builder(
                logger, active.aliases, active
            ).get_routes()
            server = WebTestHttpd(
                host="127.0.0.1",
                port=0,
                doc_root=wpt_root,
                routes=routes,
                # The harness serves files itself and forwards only a missing
                # path here, so this server also supplies the aliases that
                # exist only as rewrites: `/resources/WebIDLParser.js` is
                # `resources/webidl2/lib/webidl2.js` on disk
                # (`tools/serve/serve.py`).
                rewrites=serve.rewrites,
                config=active,
                bind_address=False,
                handler_cls=SchemeAwareHandler,
            )
            server.start()
            print(f"PORT {server.port}", flush=True)

            def stop(_signum=None, _frame=None):
                server.stop()
                raise SystemExit(0)

            signal.signal(signal.SIGTERM, stop)
            signal.signal(signal.SIGINT, stop)

            try:
                # The harness holds the write end; EOF means it is gone.
                for _line in sys.stdin:
                    pass
            finally:
                server.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
