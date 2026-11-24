import { chromium } from "@playwright/test";

import { expect, test } from "./fixtures";

test("page globals and prototypes do not expose browser internals", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const exposed = await page.evaluate(() => {
      const objects = [globalThis, document, new EventTarget(), new MessageChannel().port1];
      const names = new Set();
      for (const object of objects) {
        for (let current = object; current !== null; current = Object.getPrototypeOf(current)) {
          for (const name of Object.getOwnPropertyNames(current)) {
            if (/^__tb|^__wd|^__(queueFetch|cancelFetch|scheduleTimeout|cancelTimeout|cookieGet|cookieSet)$/.test(name)) {
              names.add(name);
            }
          }
        }
      }
      return Array.from(names).sort();
    });
    expect(exposed).toEqual([]);
  } finally {
    await browser.close();
  }
});

test("debugger handles do not publish their storage to page code", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const handle = await page.evaluateHandle(() => ({ value: 42 }));
    expect(await handle.evaluate(object => object.value)).toBe(42);
    expect(await page.evaluate(() => Object.getOwnPropertyNames(globalThis).filter(name => /^__tb/.test(name)).sort())).toEqual([]);
    expect(await page.evaluate(() => [typeof host, typeof initializeBrowser])).toEqual(["undefined", "undefined"]);
    await handle.dispose();
  } finally {
    await browser.close();
  }
});

test("shim objects have no discoverable backing storage", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const exposed = await page.evaluate(() => {
      const objects = [
        new Headers(), new Request("http://example.test/"), new Response(),
        new Blob(), new File([], "file"), new TextDecoder(), new ReadableStream(),
        new XMLHttpRequest(), new Event("test"), new CustomEvent("test"),
        new MessageEvent("test"), new MouseEvent("test"),
        new ErrorEvent("test"), new AbortController().signal,
        new URL("http://example.test/"), new URLSearchParams("a=b"),
      ];
      const exposed = [];
      for (const object of objects) {
        for (const key of Reflect.ownKeys(object)) {
          if ((typeof key === "symbol" && key !== Symbol.toStringTag && key !== Symbol.iterator)
            || (typeof key === "string" && /^_|^__tb/.test(key))) {
            exposed.push(String(key));
          }
        }
      }
      return exposed.sort();
    });
    expect(exposed).toEqual([]);
  } finally {
    await browser.close();
  }
});

test("navigation replaces private realm state without publishing it", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    for (const suffix of ["first", "second"]) {
      await page.goto(`${daemon.pageUrl}?${suffix}`);
      await page.waitForFunction(() => window.ready === true);
      expect(await page.evaluate(() => window.payload)).toBe("payload");
      expect(await page.evaluate(() => Object.getOwnPropertyNames(globalThis).filter(name => /^__tb/.test(name)).sort())).toEqual([]);
    }
  } finally {
    await browser.close();
  }
});

test("isolated worlds share document state, focus and events with the page", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.setContent(`
      <input id="input" value="some value">
      <input id="checkbox" type="checkbox">
    `);
    const session = await page.context().newCDPSession(page);
    const { frameTree } = await session.send("Page.getFrameTree");
    const { executionContextId } = await session.send("Page.createIsolatedWorld", {
      frameId: frameTree.frame.id,
      worldName: "probe",
    });
    const evaluateInWorld = async expression => {
      const { result, exceptionDetails } = await session.send("Runtime.evaluate", {
        contextId: executionContextId,
        expression,
        returnByValue: true,
      });
      expect(exceptionDetails).toBeUndefined();
      return result.value;
    };

    await evaluateInWorld(`
      window.__events = [];
      window.addEventListener("click", event => {
        window.__events.push(event.type);
        event.preventDefault();
      }, true);
      document.querySelector("#input").select();
      document.querySelector("#input").focus();
    `);

    const pageState = await page.evaluate(() => {
      const input = document.querySelector("#input");
      const checkbox = document.querySelector("#checkbox");
      const focused = document.activeElement === input;
      const selection = [input.selectionStart, input.selectionEnd];
      checkbox.click();
      return { focused, selection, checked: checkbox.checked };
    });
    expect(pageState).toEqual({ focused: true, selection: [0, 10], checked: false });
    expect(JSON.parse(await evaluateInWorld("JSON.stringify(window.__events)"))).toEqual(["click"]);

    const { root } = await session.send("DOM.getDocument", {});
    const { nodeId } = await session.send("DOM.querySelector", { nodeId: root.nodeId, selector: "#input" });
    const { object } = await session.send("DOM.resolveNode", { nodeId, executionContextId });
    expect(object.objectId).toMatch(/^iw:probe:/);
    const { result } = await session.send("Runtime.callFunctionOn", {
      objectId: object.objectId,
      functionDeclaration: "function () { this.select(); return this.selectionEnd; }",
      returnByValue: true,
    });
    expect(result.value).toBe(10);
    expect(await page.evaluate(() => document.querySelector("#input").selectionEnd)).toBe(10);
    expect(await page.evaluate(() => window.__events)).toBeUndefined();
  } finally {
    await browser.close();
  }
});

test("replaced event constructors cannot choose trusted delivery objects", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.pageUrl);
    const result = await page.evaluate(() => new Promise(resolve => {
      const Original = MessageEvent;
      let interceptions = 0;
      const forged = new Original("message", { data: "forged" });
      globalThis.MessageEvent = function() { interceptions++; return forged; };
      addEventListener("message", event => {
        globalThis.MessageEvent = Original;
        resolve({ data: event.data, trusted: event.isTrusted, interceptions, forgedTrusted: forged.isTrusted });
      }, { once: true });
      postMessage({ text: "genuine" }, "*");
    }));
    expect(result).toEqual({ data: { text: "genuine" }, trusted: true, interceptions: 0, forgedTrusted: false });
  } finally {
    await browser.close();
  }
});

test("native capabilities do not pass through replaced Function.apply", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const leaked = await page.evaluate(() => {
      const original = Function.prototype.apply;
      const apply = Reflect.apply;
      let captures = 0;
      Function.prototype.apply = function(receiver, argumentsList) {
        captures++;
        return apply(original, this, [receiver, argumentsList]);
      };
      try { new CustomEvent("test"); document.createElement("span"); }
      finally { Function.prototype.apply = original; }
      return captures;
    });
    expect(leaked).toBe(0);
  } finally {
    await browser.close();
  }
});

test("stream callbacks receive no backing-state receiver", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const receivers = await page.evaluate(async () => {
      const source = {
        pull(controller) { this.received = true; controller.close(); },
        cancel() { this.received = true; },
      };
      const stream = new ReadableStream(source);
      await stream.getReader().read();
      const pulled = source.received === true;
      source.received = false;
      await stream.cancel();
      return { pulled, canceled: source.received === true };
    });
    expect(receivers).toEqual({ pulled: true, canceled: true });
  } finally {
    await browser.close();
  }
});

test("replaced collection methods cannot observe hidden header storage", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(() => {
      const headers = new Headers({ original: "value" });
      const apply = Reflect.apply;
      const originals = { push: Array.prototype.push, filter: Array.prototype.filter, map: Array.prototype.map };
      let captures = 0;
      for (const name of Object.keys(originals)) {
        Array.prototype[name] = function(...args) { captures++; return apply(originals[name], this, args); };
      }
      let value;
      try { headers.append("extra", "added"); value = headers.get("extra"); }
      finally { for (const name of Object.keys(originals)) Array.prototype[name] = originals[name]; }
      return { captures, value };
    });
    expect(result).toEqual({ captures: 0, value: "added" });
  } finally {
    await browser.close();
  }
});

test("same-origin frame methods recognize objects without exposing slots", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.pageUrl);
    await page.evaluate(() => {
      const frame = document.createElement("iframe");
      frame.src = "/plain";
      document.body.appendChild(frame);
    });
    await page.waitForFunction(() => frames[0]?.document.readyState === "complete");
    const result = await page.evaluate(() => {
      const frame = frames[0];
      const blob = new Blob(["private"]);
      const size = Object.getOwnPropertyDescriptor(frame.Blob.prototype, "size").get;
      return {
        size: Reflect.apply(size, blob, []),
        keys: Reflect.ownKeys(blob).map(String),
        internals: Object.getOwnPropertyNames(frame).filter(name => /^__tb|^__wd/.test(name)),
      };
    });
    expect(result).toEqual({ size: 7, keys: [], internals: [] });
  } finally {
    await browser.close();
  }
});

test("debugger tables bypass page-replaced Object.create", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.evaluate(() => {
      globalThis.originalCreate = Object.create;
      globalThis.capturedTables = [];
      Object.create = function(prototype) {
        const table = originalCreate(prototype);
        if (prototype === null) capturedTables[capturedTables.length] = table;
        return table;
      };
    });
    const handle = await page.evaluateHandle(() => ({ value: 42 }));
    const captured = await page.evaluate(() => {
      Object.create = originalCreate;
      return capturedTables.map(table => Object.keys(table));
    });
    expect(captured).toEqual([]);
    await handle.dispose();
  } finally {
    await browser.close();
  }
});

test("constructor and iterator hooks cannot capture stored header pairs or blob bytes", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(async () => {
      const headers = new Headers({ original: "value" });
      const blob = new Blob(["original"]);
      const params = new URLSearchParams("a=b");
      const arrayDescriptor = Object.getOwnPropertyDescriptor(Array.prototype, "constructor");
      const bytesDescriptor = Object.getOwnPropertyDescriptor(Uint8Array.prototype, "constructor");
      const iteratorPrototype = Object.getPrototypeOf([][Symbol.iterator]());
      const next = iteratorPrototype.next;
      const apply = Reflect.apply;
      let captures = 0;
      Object.defineProperty(Array.prototype, "constructor", { configurable: true, get() { captures++; return Array; } });
      Object.defineProperty(Uint8Array.prototype, "constructor", { configurable: true, get() { captures++; return Uint8Array; } });
      iteratorPrototype.next = function() { captures++; return apply(next, this, []); };
      let bytes;
      let value;
      try { headers.entries(); bytes = blob.bytes(); value = params.get("a"); }
      finally {
        Object.defineProperty(Array.prototype, "constructor", arrayDescriptor);
        Object.defineProperty(Uint8Array.prototype, "constructor", bytesDescriptor);
        iteratorPrototype.next = next;
      }
      return { captures, value, bytes: Array.from(await bytes) };
    });
    expect(result).toEqual({ captures: 0, value: "b", bytes: [111, 114, 105, 103, 105, 110, 97, 108] });
  } finally {
    await browser.close();
  }
});

test("deletion and inherited setters cannot expose retained collection lists", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(() => {
      const headers = new Headers({ keep: "original", remove: "unused" });
      const params = new URLSearchParams("keep=original&remove=unused");
      const form = new FormData();
      form.append("keep", "original");
      form.append("remove", "unused");
      const descriptor = Object.getOwnPropertyDescriptor(Array.prototype, "constructor");
      let captures = 0;
      Object.defineProperty(Array.prototype, "constructor", {
        configurable: true,
        get() { captures++; this[0][1] = "forged"; return Array; },
      });
      try { headers.delete("remove"); params.delete("remove"); form.delete("remove"); }
      finally { Object.defineProperty(Array.prototype, "constructor", descriptor); }
      Object.defineProperty(Array.prototype, "1", {
        configurable: true,
        set(value) {
          captures++;
          this[0][1] = "forged";
          Object.defineProperty(this, "1", { value, writable: true, enumerable: true, configurable: true });
        },
      });
      try { headers.append("extra", "added"); params.append("extra", "added"); form.append("extra", "added"); }
      finally { delete Array.prototype[1]; }
      return { captures, headers: headers.get("keep"), params: params.get("keep"), form: form.get("keep") };
    });
    expect(result).toEqual({ captures: 0, headers: "original", params: "original", form: "original" });
  } finally {
    await browser.close();
  }
});

test("blob decoding and FormData iteration do not call replaced storage methods", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(async () => {
      const blob = new Blob(["original"]);
      const form = new FormData();
      form.append("keep", "original");
      const bytesSlice = Uint8Array.prototype.slice;
      const arraySlice = Array.prototype.slice;
      const arrayPush = Array.prototype.push;
      const apply = Reflect.apply;
      let captures = 0;
      Uint8Array.prototype.slice = function(...args) { captures++; return apply(bytesSlice, this, args); };
      Array.prototype.slice = function(...args) { captures++; return apply(arraySlice, this, args); };
      Array.prototype.push = function(...args) { captures++; return apply(arrayPush, this, args); };
      let text;
      let entry;
      try { text = blob.text(); form.append("extra", "added"); entry = form.entries().next().value; }
      finally {
        Uint8Array.prototype.slice = bytesSlice;
        Array.prototype.slice = arraySlice;
        Array.prototype.push = arrayPush;
      }
      return { captures, text: await text, entry };
    });
    expect(result).toEqual({ captures: 0, text: "original", entry: ["keep", "original"] });
  } finally {
    await browser.close();
  }
});

test("input delivery ignores replaced constructors and superclass links", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.interactiveUrl);
    await page.evaluate(() => {
      const OriginalMouseEvent = MouseEvent;
      const OriginalPointerEvent = PointerEvent;
      globalThis.forgedInput = new MouseEvent("click");
      globalThis.constructorCaptures = 0;
      globalThis.inputDeliveries = [];
      globalThis.MouseEvent = function() { constructorCaptures++; return forgedInput; };
      Object.setPrototypeOf(OriginalPointerEvent, function() { constructorCaptures++; return forgedInput; });
      document.querySelector("#go").addEventListener("click", event => {
        inputDeliveries.push({ forged: event === forgedInput, trusted: event.isTrusted });
      });
      globalThis.restoreInput = () => { globalThis.MouseEvent = OriginalMouseEvent; Object.setPrototypeOf(OriginalPointerEvent, OriginalMouseEvent); };
    });
    await page.locator("#go").click();
    const result = await page.evaluate(() => {
      restoreInput();
      return { captures: constructorCaptures, deliveries: inputDeliveries, forgedTrusted: forgedInput.isTrusted };
    });
    expect(result).toEqual({ captures: 0, deliveries: [{ forged: false, trusted: true }], forgedTrusted: false });
  } finally {
    await browser.close();
  }
});

test("private registries do not use replaced Map and Set methods", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.pageUrl);
    const result = await page.evaluate(() => {
      const apply = Reflect.apply;
      const mapSet = Map.prototype.set;
      const setAdd = Set.prototype.add;
      let captures = 0;
      Map.prototype.set = function(...args) { captures++; return apply(mapSet, this, args); };
      Set.prototype.add = function(...args) { captures++; return apply(setAdd, this, args); };
      let name;
      try {
        class Example extends HTMLElement {}
        customElements.define("x-private-registry", Example);
        name = customElements.get("x-private-registry").name;
        localStorage.setItem("probe", "original");
      } finally { Map.prototype.set = mapSet; Set.prototype.add = setAdd; }
      return { captures, name, storage: localStorage.getItem("probe") };
    });
    expect(result).toEqual({ captures: 0, name: "Example", storage: "original" });
  } finally {
    await browser.close();
  }
});

test("debugger metadata bypasses replaced JSON serialization and inherited toJSON", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.evaluate(() => {
      globalThis.originalStringify = JSON.stringify;
      globalThis.metadataCaptures = 0;
      JSON.stringify = function(value, ...args) {
        if (value && typeof value.objectId === "string") metadataCaptures++;
        return Reflect.apply(originalStringify, JSON, [value, ...args]);
      };
      Object.prototype.toJSON = function() {
        if (typeof this.objectId === "string") metadataCaptures++;
        return this;
      };
    });
    const handle = await page.evaluateHandle(() => ({ value: 42 }));
    const captures = await page.evaluate(() => {
      JSON.stringify = originalStringify;
      delete Object.prototype.toJSON;
      return metadataCaptures;
    });
    expect(captures).toBe(0);
    expect(await handle.evaluate(object => object.value)).toBe(42);
    await handle.dispose();
  } finally {
    await browser.close();
  }
});

test("inherited descriptor hooks cannot intercept private element creation", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(() => {
      const headers = new Headers();
      let captures = 0;
      let error = null;
      Object.defineProperty(Object.prototype, "get", {
        __proto__: null,
        configurable: true,
        get() { captures++; return undefined; },
      });
      try { headers.append("keep", "original"); }
      catch (caught) { error = String(caught); }
      finally { delete Object.prototype.get; }
      return { captures, error, value: headers.get("keep") };
    });
    expect(result).toEqual({ captures: 0, error: null, value: "original" });
  } finally {
    await browser.close();
  }
});

test("replaced global constructors do not expose proxy handlers or change delivered event types", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.pageUrl);
    const result = await page.evaluate(() => new Promise(resolve => {
      const OriginalProxy = Proxy;
      const OriginalString = String;
      let proxyCaptures = 0;
      globalThis.Proxy = function(target, handler) { proxyCaptures++; return new OriginalProxy(target, handler); };
      globalThis.String = function() { return "forged"; };
      const storage = localStorage;
      storage.setItem("probe", "original");
      addEventListener("message", event => {
        globalThis.Proxy = OriginalProxy;
        globalThis.String = OriginalString;
        resolve({ proxyCaptures, type: event.type, data: event.data, storage: storage.getItem("probe") });
      }, { once: true });
      postMessage("genuine", "*");
    }));
    expect(result).toEqual({ proxyCaptures: 0, type: "message", data: "genuine", storage: "original" });
  } finally {
    await browser.close();
  }
});

test("internal scheduling bypasses replaced public timer and microtask functions", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(async () => {
      const originalTimeout = setTimeout;
      const originalMicrotask = queueMicrotask;
      let captures = 0;
      globalThis.setTimeout = function(...args) { captures++; return originalTimeout(...args); };
      globalThis.queueMicrotask = function(callback) { captures++; return originalMicrotask(callback); };
      const done = [];
      try {
        await Promise.all([
          new Promise(resolve => AbortSignal.timeout(0).addEventListener("abort", () => { done.push("abort"); resolve(); })),
          new Promise(resolve => requestAnimationFrame(() => { done.push("frame"); resolve(); })),
          new Promise(resolve => {
            const reader = new FileReader();
            reader.onload = () => { done.push(reader.result); resolve(); };
            reader.readAsText(new Blob(["original"]));
          }),
          new Promise(resolve => {
            const observer = new IntersectionObserver(() => { done.push("observer"); observer.disconnect(); resolve(); });
            observer.observe(document.documentElement);
          }),
        ]);
      } finally { globalThis.setTimeout = originalTimeout; globalThis.queueMicrotask = originalMicrotask; }
      return { captures, done: done.sort() };
    });
    expect(result).toEqual({ captures: 0, done: ["abort", "frame", "observer", "original"] });
  } finally {
    await browser.close();
  }
});

test("XHR state and received bytes bypass inherited and typed-array hooks", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.pageUrl);
    const result = await page.evaluate(async () => {
      const xhr = new XMLHttpRequest();
      let stateCaptures = 0;
      Object.defineProperty(Object.prototype, "overrideMimeType", {
        __proto__: null, configurable: true,
        set() { stateCaptures++; },
      });
      try { xhr.overrideMimeType("text/plain"); }
      finally { delete Object.prototype.overrideMimeType; }
      const OriginalHeaders = Headers;
      const originalGet = Headers.prototype.get;
      const originalAppend = Headers.prototype.append;
      let headerCaptures = 0;
      globalThis.Headers = function(...args) { headerCaptures++; return new OriginalHeaders(...args); };
      OriginalHeaders.prototype.get = function(...args) { headerCaptures++; return Reflect.apply(originalGet, this, args); };
      OriginalHeaders.prototype.append = function(...args) { headerCaptures++; return Reflect.apply(originalAppend, this, args); };
      xhr.open("GET", "/data");
      xhr.setRequestHeader("X-Probe", "original");
      xhr.responseType = "arraybuffer";
      await new Promise((resolve, reject) => { xhr.onload = resolve; xhr.onerror = reject; xhr.send(); });
      const contentType = xhr.getResponseHeader("content-type");
      globalThis.Headers = OriginalHeaders;
      OriginalHeaders.prototype.get = originalGet;
      OriginalHeaders.prototype.append = originalAppend;
      const originalSlice = Uint8Array.prototype.slice;
      const originalLength = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(Uint8Array.prototype), "length");
      const apply = Reflect.apply;
      let byteCaptures = 0;
      Uint8Array.prototype.slice = function(...args) { byteCaptures++; return apply(originalSlice, this, args); };
      Object.defineProperty(Object.getPrototypeOf(Uint8Array.prototype), "length", {
        __proto__: null, configurable: true,
        get() { byteCaptures++; return apply(originalLength.get, this, []); },
      });
      let response;
      try { response = xhr.response; }
      finally {
        Uint8Array.prototype.slice = originalSlice;
        Object.defineProperty(Object.getPrototypeOf(Uint8Array.prototype), "length", originalLength);
      }
      const first = new Uint8Array(response);
      const text = new TextDecoder().decode(first);
      first[0] = 0;
      return { stateCaptures, byteCaptures, headerCaptures, contentType, text, retained: new TextDecoder().decode(xhr.response) };
    });
    expect(result).toEqual({ stateCaptures: 0, byteCaptures: 0, headerCaptures: 0, contentType: "text/plain", text: "payload", retained: "payload" });
  } finally {
    await browser.close();
  }
});

test("borrowed Window listeners retain foreign global identity", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    await page.goto(daemon.pageUrl);
    const result = await page.evaluate(async () => {
      const frame = document.createElement("iframe");
      const loaded = new Promise(resolve => { frame.onload = resolve; });
      frame.src = location.href;
      document.body.appendChild(frame);
      await loaded;
      const foreign = frame.contentWindow.eval("globalThis");
      const deliveries = [];
      const callback = () => deliveries.push("child");
      const event = new Event("probe");
      Reflect.apply(addEventListener, foreign, ["probe", callback]);
      dispatchEvent(event);
      const parentCount = deliveries.length;
      Reflect.apply(dispatchEvent, foreign, [event]);
      const childCount = deliveries.length;
      Reflect.apply(removeEventListener, foreign, ["probe", callback]);
      Reflect.apply(dispatchEvent, foreign, [event]);
      return { parentCount, childCount, removedCount: deliveries.length };
    });
    expect(result).toEqual({ parentCount: 0, childCount: 1, removedCount: 1 });
  } finally {
    await browser.close();
  }
});

test("private FileList iteration bypasses replaced array iterators and stays exhausted", async ({ daemon }) => {
  const browser = await chromium.connectOverCDP(daemon.origin);
  try {
    const page = browser.contexts()[0].pages()[0];
    const result = await page.evaluate(() => {
      const transfer = new DataTransfer();
      const first = new File([], "first");
      const second = new File([], "second");
      transfer.items.add(first);
      const original = Array.prototype[Symbol.iterator];
      let captures = 0;
      Array.prototype[Symbol.iterator] = function() { captures++; return Reflect.apply(original, this, []); };
      try {
        const iterator = transfer.files[Symbol.iterator]();
        const firstName = iterator.next().value.name;
        const finished = iterator.next().done;
        transfer.items.add(second);
        return { captures, firstName, finished, stillFinished: iterator.next().done, length: transfer.files.length };
      } finally { Array.prototype[Symbol.iterator] = original; }
    });
    expect(result).toEqual({ captures: 0, firstName: "first", finished: true, stillFinished: true, length: 2 });
  } finally {
    await browser.close();
  }
});
