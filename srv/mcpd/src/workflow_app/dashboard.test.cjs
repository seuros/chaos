// Bounded bridge/DOM tests. Run: node --test srv/mcpd/src/workflow_app/dashboard.test.cjs
// No browser, packages, network, or Cargo required.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const html = fs.readFileSync(path.join(__dirname, "dashboard.html"), "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const RUN_ID = "f849dd49-2143-43b5-a6d1-93ab21c1a348";
const tick = () => new Promise((resolve) => setImmediate(resolve));

class Element {
  constructor(tag) {
    this.tag = tag;
    this.children = [];
    this.listeners = new Map();
    this.disabled = false;
    this.value = "";
  }
  set textContent(value) { this.value = String(value); this.children = []; }
  get textContent() { return this.value + this.children.map((node) => node.textContent).join(" "); }
  set innerHTML(_) { throw new Error("Unsafe DOM sink"); }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) { this.value = ""; this.children = nodes; }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  removeEventListener(name) { this.listeners.delete(name); }
  querySelectorAll(tag) {
    return this.children.flatMap((child) => [
      ...(child.tag === tag ? [child] : []), ...child.querySelectorAll(tag),
    ]);
  }
  click() { if (!this.disabled) return this.listeners.get("click")?.(); }
}

function harness({ embedded = true } = {}) {
  const posts = [];
  const listeners = new Map();
  const timers = new Map();
  const nodes = Object.fromEntries(["refresh", "next", "status", "notice", "runs"].map((id) => [id, new Element(id)]));
  nodes.refresh.disabled = true;
  nodes.next.disabled = true;
  const parent = { postMessage: (message, origin) => posts.push({ message, origin }) };
  const window = {
    parent, innerWidth: 720,
    addEventListener: (name, fn) => listeners.set(name, fn),
    removeEventListener: (name) => listeners.delete(name),
  };
  if (!embedded) window.parent = window;
  let timerId = 0;
  let disconnected = 0;
  const context = {
    window,
    document: {
      getElementById: (id) => nodes[id],
      createElement: (tag) => new Element(tag),
      body: new Element("body"),
      documentElement: { getBoundingClientRect: () => ({ height: 400 }) },
    },
    setTimeout: (fn) => {
      const id = ++timerId;
      timers.set(id, () => { timers.delete(id); fn(); });
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
    ResizeObserver: class {
      observe() {}
      disconnect() { disconnected++; }
    },
  };
  vm.runInNewContext(script, context, { timeout: 1000 });
  const receive = (data, options = {}) => listeners.get("message")?.({
    source: options.source ?? parent,
    origin: options.origin ?? "https://host.test",
    data: { jsonrpc: "2.0", ...data },
  });
  const calls = () => posts.filter((post) => post.message.method === "tools/call");
  const ready = async (capabilities = { serverTools: {} }) => {
    const init = posts.find((post) => post.message.method === "ui/initialize").message;
    receive({ id: init.id, result: { protocolVersion: "2026-01-26", hostCapabilities: capabilities } });
    await tick();
  };
  const toolResult = (payload) => receive({
    method: "ui/notifications/tool-result", params: { structuredContent: payload },
  });
  return { posts, listeners, timers, nodes, receive, ready, calls, toolResult, disconnected: () => disconnected };
}

function run(overrides = {}) {
  return {
    run_id: RUN_ID, kind: "workflow", status: { state: "running" }, truncated: false,
    steps: [
      { node_id: "request", attempt: 1, status: { state: "terminal", outcome: "failure" }, truncated: true },
      { node_id: "request", attempt: 2, status: { state: "waiting_for_input" }, truncated: false },
    ],
    ...overrides,
  };
}

test("handshake is exact, bounded, parent-only, and never auto-queries", async () => {
  const h = harness();
  assert.equal(h.posts.length, 1);
  const init = h.posts[0].message;
  assert.equal(init.method, "ui/initialize");
  assert.equal(init.params.protocolVersion, "2026-01-26");
  assert.equal(JSON.stringify(init.params.appCapabilities), "{}");
  h.receive({ id: init.id, result: {} }, { source: {} });
  await tick();
  assert.equal(h.timers.size, 1);
  h.toolResult({ runs: [run()] });
  assert.equal(h.nodes.runs.children.length, 0);
  await h.ready();
  assert.equal(h.timers.size, 0);
  assert.equal(h.calls().length, 0);
  assert.ok(h.posts.some((post) => post.message.method === "ui/notifications/initialized"));
  assert.ok(h.posts.some((post) => post.message.method === "ui/notifications/size-changed"));
  assert.equal(h.nodes.refresh.disabled, false);
  h.toolResult({ runs: [run()] });
  assert.equal(h.nodes.runs.querySelectorAll("article").length, 1);
  h.receive({
    method: "ui/notifications/tool-result", params: { structuredContent: { runs: [] } },
  }, { origin: "https://other.test" });
  assert.equal(h.nodes.runs.querySelectorAll("article").length, 1);
});

test("refresh, manual pagination and cancel use only their fixed tools", async () => {
  const h = harness();
  await h.ready();
  h.nodes.refresh.click();
  let call = h.calls().at(-1).message;
  assert.equal(call.params.name, "workflow_dashboard");
  assert.equal(JSON.stringify(call.params.arguments), "{}");
  h.receive({ id: call.id, result: { structuredContent: { runs: [run()], next_offset: 10, details_omitted: true } } });
  await tick();
  assert.equal(h.nodes.next.disabled, false);
  assert.match(h.nodes.notice.textContent, /intentionally omitted/);
  h.nodes.next.click();
  call = h.calls().at(-1).message;
  assert.equal(JSON.stringify(call.params.arguments), '{"offset":10}');
  h.receive({ id: call.id, result: { structuredContent: { runs: [run()], details_omitted: true } } });
  await tick();
  assert.match(h.nodes.status.textContent, /offset 10/);
  assert.equal(h.nodes.next.disabled, true);
  const cancel = h.nodes.runs.querySelectorAll("button")[0];
  cancel.click();
  call = h.calls().at(-1).message;
  assert.equal(call.params.name, "cancel_workflow");
  assert.equal(JSON.stringify(call.params.arguments), JSON.stringify({ run_id: RUN_ID }));
  assert.equal(cancel.disabled, true);
  h.receive({ id: call.id, result: { structuredContent: run({ status: { state: "cancelling" } }) } });
  await tick();
  assert.match(h.nodes.runs.textContent, /cleanup pending/);
  assert.equal(h.nodes.runs.querySelectorAll("button").length, 0);
  h.nodes.refresh.click();
  assert.equal(JSON.stringify(h.calls().at(-1).message.params.arguments), "{}");
});

test("fixed schema safely shows attempts, pending, outcomes and truncation", async () => {
  const h = harness();
  await h.ready();
  h.toolResult({ runs: [run({
    status: { state: "terminal", outcome: "failure" }, truncated: true,
    steps: [...run().steps, { node_id: "<img src=x onerror=alert(1)>", attempt: 3, status: { state: "running" } }],
    result: "<script>private result</script>",
  })] });
  const content = h.nodes.runs.textContent;
  assert.match(content, /Pending input/);
  assert.match(content, /failure/);
  assert.match(content, /server discarded/);
  assert.match(content, /onerror=alert/); // Literal text, not markup.
  assert.equal(h.nodes.runs.querySelectorAll("img").length, 0);
  assert.equal(h.nodes.runs.querySelectorAll("script").length, 0);
  assert.ok(!content.includes("private result"));
  assert.equal(h.nodes.runs.querySelectorAll("button").length, 0);
});

test("rendering has global DOM limits and ignores arbitrary details", async () => {
  const h = harness();
  await h.ready();
  const many = run({ steps: Array.from({ length: 300 }, (_, i) => ({
    node_id: "x".repeat(1000), attempt: i + 1, status: { state: "running" }, detail: "SECRET",
  })) });
  h.toolResult({ runs: Array.from({ length: 20 }, () => many), next_offset: 10 });
  assert.equal(h.nodes.runs.querySelectorAll("article").length, 10);
  assert.ok(h.nodes.runs.querySelectorAll("td").length <= 512 * 5);
  assert.match(h.nodes.notice.textContent, /Display truncated/);
  assert.ok(!h.nodes.runs.textContent.includes("SECRET"));
  assert.ok(h.nodes.runs.textContent.length < 200_000);
});

test("missing capabilities leave tool buttons disabled without inventing support", async () => {
  const h = harness();
  await h.ready({});
  h.toolResult({ runs: [run()], next_offset: 10 });
  h.nodes.refresh.click();
  h.nodes.next.click();
  h.nodes.runs.querySelectorAll("button")[0].click();
  assert.equal(h.calls().length, 0);
  assert.match(h.nodes.status.textContent, /Showing 1/);
  assert.equal(h.nodes.refresh.disabled, true);
});

test("ping and unknown requests are answered without tool dispatch", async () => {
  const h = harness();
  await h.ready();
  h.receive({ id: "ping-1", method: "ping", params: {} });
  h.receive({ id: "unknown-1", method: "arbitrary/renderer", params: {} });
  assert.equal(JSON.stringify(h.posts.find((post) => post.message.id === "ping-1").message.result), "{}");
  assert.equal(h.posts.find((post) => post.message.id === "unknown-1").message.error.code, -32601);
  assert.equal(h.calls().length, 0);
  assert.equal(h.timers.size, 0);
});

test("in-flight operations are serialized and timeout never implies cancellation finished", async () => {
  const h = harness();
  await h.ready();
  h.toolResult({ runs: [run()] });
  h.nodes.runs.querySelectorAll("button")[0].click();
  h.nodes.refresh.click();
  assert.equal(h.calls().length, 1);
  assert.equal(h.timers.size, 1);
  const call = h.calls()[0].message;
  h.receive({ id: call.id, result: {}, error: { message: "ambiguous response" } });
  assert.equal(h.timers.size, 1);
  for (const fire of h.timers.values()) fire();
  await tick();
  assert.match(h.nodes.status.textContent, /timed out; refresh/);
  assert.match(h.nodes.runs.textContent, /Outcome: not terminal/);
  assert.equal(h.nodes.refresh.disabled, false);
  assert.equal(h.timers.size, 0);
});

test("teardown acknowledges, drops data, rejects calls and disconnects", async () => {
  const h = harness();
  await h.ready();
  h.toolResult({ runs: [run()] });
  h.nodes.refresh.click();
  assert.equal(h.timers.size, 1);
  h.receive({ id: "close-1", method: "ui/resource-teardown", params: {} });
  await tick();
  assert.ok(h.posts.some((post) => post.message.id === "close-1" && post.message.result));
  assert.equal(h.timers.size, 0);
  assert.equal(h.disconnected(), 1);
  assert.equal(h.listeners.has("message"), false);
  assert.equal(h.listeners.has("pagehide"), false);
  assert.equal(h.nodes.refresh.disabled, true);
  assert.equal(h.nodes.next.disabled, true);
  assert.ok(!h.nodes.runs.textContent.includes(RUN_ID));
  assert.match(h.nodes.status.textContent, /Disconnected/);
});

test("plain tool text, malformed payloads and empty pages have useful fallbacks", async () => {
  const h = harness();
  await h.ready();
  h.toolResult({ runs: [] });
  assert.match(h.nodes.runs.textContent, /No runs on this page/);
  h.receive({ method: "ui/notifications/tool-result", params: { content: [{ type: "text", text: "Read workflow_status instead" }] } });
  assert.match(h.nodes.status.textContent, /Read workflow_status/);
  h.receive({ method: "ui/notifications/tool-result", params: { isError: true, content: [{ type: "text", text: "Unknown run" }] } });
  assert.match(h.nodes.status.textContent, /Tool error: Unknown run/);
  h.toolResult({ runs: [null, { run_id: "bad" }], next_offset: -1 });
  assert.equal(h.nodes.runs.querySelectorAll("article").length, 0);
  assert.equal(h.nodes.next.disabled, true);
  assert.match(html, /<noscript>[\s\S]*workflow_status/);
});

test("standalone, unsupported version and timeout do not enable tools", async () => {
  const standalone = harness({ embedded: false });
  assert.equal(standalone.posts.length, 0);
  assert.match(standalone.nodes.status.textContent, /host unavailable/);
  const mismatch = harness();
  mismatch.receive({ id: mismatch.posts[0].message.id, result: { protocolVersion: "future", hostCapabilities: { serverTools: {} } } });
  await tick();
  assert.match(mismatch.nodes.status.textContent, /handshake failed/);
  assert.ok(!mismatch.posts.some((post) => post.message.method === "ui/notifications/initialized"));
  const timeout = harness();
  for (const fire of timeout.timers.values()) fire();
  await tick();
  assert.match(timeout.nodes.status.textContent, /timed out/);
  assert.equal(timeout.nodes.refresh.disabled, true);
  assert.equal(timeout.calls().length, 0);
});
