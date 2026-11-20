// The demo page's driver: mount the document, then step the engine until it is
// idle, showing what it reported.

import { engine } from "./generated/tinybrowser.js";
import { configure, inFlight } from "./host.mjs";

const urlInput = document.querySelector("#url");
const htmlInput = document.querySelector("#html");
const loadButton = document.querySelector("#load");
const statusLine = document.querySelector("#status");
const engineOutput = document.querySelector("#engine");
const exchanges = document.querySelector("#exchanges");

let tab = null;
/// Resolves the wait for the next host exchange, when the page is idle.
let wake = null;

configure({
  onSettled: ({ id, outcome }) => {
    const status = outcome.tag === "ok" ? outcome.val.status : outcome.val;
    exchanges.textContent += `${id} ${status}\n`;
  },
  onWake: () => {
    if (wake === null) {
      return;
    }
    const resume = wake;
    wake = null;
    resume();
  },
});

loadButton.addEventListener("click", () => {
  try {
    tab = new engine.Tab(urlInput.value, htmlInput.value);
  } catch (error) {
    engineOutput.textContent = `mount failed: ${error}`;
    return;
  }
  exchanges.textContent = "";
  statusLine.textContent = "running";
  pump();
});

async function pump() {
  // `step` answers the milliseconds until the next timer, zero when ready work
  // is still queued, and nothing when the page is idle. An idle page with an
  // exchange in flight is not finished: the host wakes it with the answer.
  for (;;) {
    const delay = tab.step();
    show();
    if (delay === undefined || delay === null) {
      if (inFlight() === 0) {
        statusLine.textContent = "idle";
        return;
      }
      await new Promise((resume) => {
        wake = resume;
      });
      continue;
    }
    const milliseconds = Number(delay);
    if (milliseconds > 0) {
      await new Promise((resume) => setTimeout(resume, milliseconds));
    }
  }
}

function show() {
  const title = attempt(() => tab.title());
  const text = attempt(() => tab.text());
  engineOutput.textContent = `title: ${title}\n\n${text}`;
}

function attempt(read) {
  try {
    return String(read());
  } catch (error) {
    return `<failed: ${error?.message ?? error}>`;
  }
}
