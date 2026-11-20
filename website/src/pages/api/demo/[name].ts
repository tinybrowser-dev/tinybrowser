import type { APIRoute } from "astro"

import { handleDemoFixture } from "@/lib/demo-fixtures"

export const prerender = false

export const GET: APIRoute = ({ params, request }) => {
  return handleDemoFixture(params.name ?? "", request.headers.get("cookie"))
}
