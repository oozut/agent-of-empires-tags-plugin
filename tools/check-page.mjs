#!/usr/bin/env node
// Run the LLM Tags page's script against a stub DOM and a stub dashboard, and
// check that the bridge actually opens.
//
// The page is one self-contained file with no dependencies, so this needs no
// browser and no Playwright: the script is evaluated in a `vm` context with a
// hand-rolled DOM, and `parent` plays the dashboard side of the handshake — wait
// for `ready`, transfer a channel end back, answer `plugins.uiState`, and read
// what the page rendered.
//
// This exists because the page shipped a handshake that threw "send is not a
// function" (a call site passed its transport where the params belonged) and a
// unit test could not see it: the failure only happened when the document ran.
//
//   tools/check-page.mjs [path/to/tags.html]
//
// Exit status is 0 only if the bridge opened and the state rendered.

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const file = resolve(process.argv[2] || resolve(here, "..", "ui", "tags.html"));
const html = readFileSync(file, "utf8");

const script = html.match(/<script>([\s\S]*?)<\/script>/);
if (!script) fail("the page has no inline <script>");

// ---- stub DOM ---------------------------------------------------------------
function element(tag = "div") {
  const node = {
    tagName: tag.toUpperCase(),
    children: [],
    _innerHTML: "",
    // A real element drops its children when `innerHTML` is assigned (notably the
    // `""` a re-render uses to clear a table); the stub must do the same or a
    // second render looks like duplicated rows.
    get innerHTML() {
      return this._innerHTML;
    },
    set innerHTML(text) {
      this._innerHTML = text;
      if (text === "") this.children.length = 0;
    },
    textContent: "",
    className: "",
    value: "",
    title: "",
    disabled: false,
    colSpan: 1,
    selectionStart: 0,
    selectionEnd: 0,
    focus() {},
    setSelectionRange(start, end) {
      this.selectionStart = start;
      this.selectionEnd = end;
    },
    style: { setProperty() {} },
    append(...kids) {
      this.children.push(...kids);
    },
    appendChild(kid) {
      this.children.push(kid);
      return kid;
    },
  };
  return node;
}

// A real `textContent` is the node's own text plus its descendants'; the stub
// elements hold both separately, so flatten before asserting on a cell.
function flatText(node) {
  if (!node) return "";
  return (node.textContent || "") + (node.children || []).map(flatText).join("");
}

const byId = new Map();
const selectors = new Map();
const document = {
  documentElement: element("html"),
  getElementById(id) {
    if (!byId.has(id)) byId.set(id, element());
    return byId.get(id);
  },
  querySelector(selector) {
    if (!selectors.has(selector)) selectors.set(selector, element());
    return selectors.get(selector);
  },
  createElement: element,
};

// ---- stub dashboard ---------------------------------------------------------
const seen = [];
const replies = [];
const channels = [];
let opened = null;

const parent = {
  postMessage(message, _origin, transfer) {
    seen.push(message);
    if (message.method !== "ready") {
      fail(`the first message the page posted was ${JSON.stringify(message)}`);
    }
    if (message.params?.nonce !== "abc123") {
      fail(`ready carried nonce ${JSON.stringify(message.params?.nonce)}, expected "abc123"`);
    }
    const [port] = transfer || [];
    if (!port) fail("ready transferred no MessageChannel port");
    opened = port;
    // The dashboard's reply travels back over the transferred port.
    port.postMessage({ aoeBridge: 1, id: message.id, result: null });
    port.postMessage({ aoeBridge: 1, event: "context", data: { readOnly: false, cityhall: false } });
    port.postMessage({ aoeBridge: 1, event: "theme", data: { cssVars: { "--fg": "#fff" } } });
    port.onmessage = ({ data }) => {
      replies.push(data);
      const reply = (result) => port.postMessage({ aoeBridge: 1, id: data.id, result });
      if (data.method === "plugins.uiState") reply(STATE);
      else reply(null);
    };
  },
};

const STATE = {
  entries: [
    {
      plugin_id: "dev.oozut.llm-tags",
      slot: "page",
      id: "tags",
      payload: {
        sessions: [
          {
            id: "cf11af87da7e4bf0",
            title: "Lithuanians",
            tool: "pi-proxy",
            path: "/tmp/sessions/cf11af87da7e4bf0/overrides.toml",
            tags: [
              { key: "team", value: "platform", origin: "session" },
              { key: "cost", value: 1.5, origin: "config" },
            ],
            unset: ["epic"],
            token: "",
            targets: [
              { name: "X-AoE-Tags", env_var: "AOE_HDR_X_AOE_TAGS", value: '{"team":"platform"}' },
            ],
          },
        ],
        references: [
          { name: "session", reference: "{session}", group: "Session", description: "Short session id." },
          { name: "git.branch", reference: "{git.branch}", group: "Git", description: "Current branch." },
        ],
        presets: [
          {
            id: "git",
            label: "Add standard git information",
            tags: ["vcs:git", "git.branch:{git.branch}"],
          },
        ],
        page: "/plugins/dev.oozut.llm-tags/tags",
      },
    },
  ],
};

class MessageChannel {
  constructor() {
    const make = () => ({ onmessage: null, postMessage: null });
    this.port1 = make();
    this.port2 = make();
    // A real channel delivers asynchronously; so does this one.
    this.port1.postMessage = (data) => queueMicrotask(() => this.port2.onmessage?.({ data }));
    this.port2.postMessage = (data) => queueMicrotask(() => this.port1.onmessage?.({ data }));
    channels.push(this);
  }
}

const context = vm.createContext({
  document,
  parent,
  location: { hash: "#abc123" },
  MessageChannel,
  confirm: () => true,
  setTimeout,
  clearTimeout,
  console,
  JSON,
  Math,
  Promise,
  Error,
  TypeError,
  String,
  Number,
  Object,
  Array,
  Map,
  RegExp,
});

function fail(message) {
  console.error(`FAIL ${message}`);
  process.exit(1);
}

try {
  new vm.Script(script[1], { filename: file }).runInContext(context);
} catch (error) {
  // The script's own `.catch` renders an error banner instead of throwing; this
  // catches a failure before that, which is also a broken page.
  fail(`the page threw while loading: ${error.message}`);
}

// Let the handshake and the `plugins.uiState` round-trip play out.
await new Promise((done) => setTimeout(done, 50));

const banner = byId.get("banner");
if (banner?.className?.includes("err")) {
  fail(`the page showed an error banner: ${banner.textContent}`);
}
if (!opened) fail("the page never opened the bridge");

const asked = replies.map((reply) => reply.method);
if (!asked.includes("plugins.uiState")) {
  fail(`the page asked the dashboard for ${JSON.stringify(asked)}, not plugins.uiState`);
}
if (asked.includes("action")) fail("the page wrote something on load");

// The session select is filled from the pushed state; its options are child
// nodes, so read those rather than the select's own text.
const options = byId.get("session")?.children || [];
const labels = options.map((option) => option.textContent);
if (!labels.some((label) => label.includes("Lithuanians"))) {
  fail(`the session select was not filled: ${JSON.stringify(labels)}`);
}
const tagRows = (selectors.get("#tags tbody")?.children || []).map((row) =>
  row.children.map(flatText).join(" | "),
);
if (!tagRows.some((row) => row.includes("team") && row.includes("this session"))) {
  fail(`the tag table is missing the session's own entry: ${JSON.stringify(tagRows)}`);
}
if (!tagRows.some((row) => row.includes("cost") && row.includes("settings"))) {
  fail(`the tag table is missing the settings entry: ${JSON.stringify(tagRows)}`);
}
const targetRows = (selectors.get("#targets tbody")?.children || []).map((row) =>
  row.children.map(flatText).join(" | "),
);
if (!targetRows.some((row) => row.includes("X-AoE-Tags") && row.includes("AOE_HDR_X_AOE_TAGS"))) {
  fail(`the target table is empty: ${JSON.stringify(targetRows)}`);
}
if (banner && banner.textContent) fail(`the page left a banner up: ${banner.textContent}`);

// The reference dropdown comes from the pushed catalogue: a Git group with the
// dotted reference in it, not a hard-coded list in the page.
const referenceOptions = (byId.get("reference")?.children || []).flatMap((node) =>
  node.children?.length ? node.children : [node],
);
if (!referenceOptions.some((option) => option.value === "{git.branch}")) {
  fail(
    `the reference dropdown is missing {git.branch}: ${JSON.stringify(
      referenceOptions.map((option) => option.value),
    )}`,
  );
}
const valueInput = document.getElementById("value");
valueInput.value = "git.branch:";
valueInput.selectionStart = valueInput.value.length;
valueInput.selectionEnd = valueInput.value.length;
byId.get("reference").onchange({ target: { value: "{git.branch}" } });
if (valueInput.value !== "git.branch:{git.branch}") {
  fail(`inserting a reference produced ${JSON.stringify(valueInput.value)}`);
}
// Inserting is a local edit: it must not have asked the worker for anything.
if (replies.some((reply) => reply.method === "action")) {
  fail("inserting a reference sent an action; it should only fill the field");
}

// The sample tick is a per-session write of the whole preset, fired once.
if (!byId.get("presetsub")?.textContent?.includes("git.branch:{git.branch}")) {
  fail(`the preset note does not name what it writes: ${byId.get("presetsub")?.textContent}`);
}
byId.get("gitpreset").onchange({ target: { checked: true } });
await new Promise((done) => setTimeout(done, 50));
const writes = replies
  .filter((reply) => reply.method === "action")
  .map((reply) => reply.params?.method);
if (!writes.includes("tag.preset")) {
  fail(`ticking the preset fired ${JSON.stringify(writes)}, not tag.preset`);
}
const presetWrite = replies.find(
  (reply) => reply.method === "action" && reply.params?.method === "tag.preset",
);
if (presetWrite?.params?.params?.preset !== "git" || presetWrite?.params?.params?.applied !== true) {
  fail(`the preset write carried ${JSON.stringify(presetWrite?.params)}`);
}

console.log(`OK ${file}`);
console.log(
  `   opened the bridge, asked for ${asked.join(", ")}, rendered ${tagRows.length} tag row(s), ` +
    `${targetRows.length} target row(s), ${referenceOptions.length} reference(s); ` +
    `the git preset write carries ${JSON.stringify(presetWrite.params.params)}`,
);
