/**
 * Self-contained fixtures under `/api/demo/*` (302 redirect, Set-Cookie with
 * HttpOnly, per-session login, and cookie echo) so the Wasm lab proves
 * redirects, isolated cookie jars, and HttpOnly cookies without an external origin.
 */
export function handleDemoFixture(
  name: string,
  cookieHeader: string | null,
  searchParams?: URLSearchParams,
): Response {
  if (name === "redirect") {
    return new Response(null, {
      status: 302,
      headers: { location: "/api/demo/hello.txt" },
    })
  }
  if (name === "hello.js") {
    const headers = new Headers({
      "content-type": "text/javascript; charset=utf-8",
    })
    headers.append("set-cookie", "tb_demo=1; Path=/")
    headers.append("set-cookie", "tb_secret=httponly_jar_token; Path=/; HttpOnly")
    return new Response('document.body.dataset.subresource = "script loaded";\n', {
      status: 200,
      headers,
    })
  }
  if (name === "login") {
    const user = (searchParams?.get("user") ?? "guest").replace(/[^a-zA-Z0-9_-]/g, "").slice(0, 24)
    const headers = new Headers({
      "content-type": "text/plain; charset=utf-8",
    })
    headers.append("set-cookie", `tb_user=${user || "guest"}; Path=/`)
    headers.append("set-cookie", `tb_session=sess_${user || "guest"}_secret; Path=/; HttpOnly`)
    return new Response(`signed in as ${user || "guest"}\n`, {
      status: 200,
      headers,
    })
  }
  if (name === "hello.txt") {
    return new Response("hello from tinybrowser demo origin\n", {
      status: 200,
      headers: {
        "content-type": "text/plain; charset=utf-8",
        "x-frame-options": "DENY",
      },
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
