import { render } from "@solidjs/web"
import { DemoApp } from "./DemoApp"

const root = document.getElementById("demo-root")
if (root) {
  root.replaceChildren()
  render(() => <DemoApp />, root)
}
