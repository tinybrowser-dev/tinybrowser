/**
 * Self-contained fixtures under `/api/demo/*` (302 redirect, Set-Cookie
 * script, cookie echo) so the Wasm demo proves redirects and cookies without
 * needing an external origin.
 */
export function handleDemoFixture(name: string, cookieHeader: string | null): Response {
  if (name === "redirect") {
    return new Response(null, {
      status: 302,
      headers: { location: "/api/demo/hello.txt" },
    })
  }
  if (name === "hello.js") {
    return new Response('document.body.dataset.subresource = "script loaded";\n', {
      status: 200,
      headers: {
        "content-type": "text/javascript; charset=utf-8",
        "set-cookie": "tb_demo=1; Path=/",
      },
    })
  }
  if (name === "hello.txt") {
    return new Response("hello from tinybrowser demo origin\n", {
      status: 200,
      headers: { "content-type": "text/plain; charset=utf-8" },
    })
  }
  if (name === "echo") {
    return new Response(
      `cookie: ${cookieHeader ?? "(none)"}\nserver_time: ${new Date().toISOString()}\n`,
      {
        status: 200,
        headers: { "content-type": "text/plain; charset=utf-8" },
      },
    )
  }
  return new Response("not found", { status: 404 })
}
