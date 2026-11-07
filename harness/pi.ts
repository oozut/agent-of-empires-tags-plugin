/**
 * aoe-llm-tags — per-request tags for this session.
 *
 * Written into this session's private config copy by the Agent of Empires
 * "LLM Tags" plugin, and only after you approved this exact file: the plugin
 * keeps a receipt with this file's SHA-256, and a change to the file stops it
 * from being installed until you approve it again.
 *
 * What it does: before each model request, re-read the session's live file and
 * set the request headers it names. That is what lets a tag edited while this
 * session runs reach the *next* request instead of the next launch.
 *
 * What it does not do: it never reads your prompts, transcript, files or
 * credentials; it opens no socket, spawns nothing, and writes nothing anywhere.
 * It is inert unless AOE_LLM_TAGS_LIVE names a readable file, so a stray copy
 * outside an aoe launch does nothing at all.
 *
 * Fail-open by construction: if the file is missing, half-written or not JSON,
 * the request goes out with the headers the launch already set. Losing a tag is
 * a shame; losing a request is not acceptable.
 */
import { readFileSync, statSync } from "node:fs";

const LIVE = process.env.AOE_LLM_TAGS_LIVE ?? "";

type Target = { kind?: string; name?: string; value?: string };
type Live = { enabled?: boolean; targets?: Target[] };

let cache: { mtimeMs: number; data: Live | null } = { mtimeMs: -1, data: null };

function live(): Live | null {
  if (!LIVE) return null;
  try {
    const stat = statSync(LIVE);
    if (stat.mtimeMs === cache.mtimeMs) return cache.data;
    const data = JSON.parse(readFileSync(LIVE, "utf8")) as Live;
    cache = { mtimeMs: stat.mtimeMs, data };
    return data;
  } catch {
    // Missing, half-written, not JSON, or unreadable: no dynamic value this
    // request, and the launch-time headers stay in place.
    cache = { mtimeMs: -1, data: null };
    return null;
  }
}

export default function aoeLlmTags(pi: any) {
  if (!LIVE) return;

  pi.on("before_provider_headers", (event: any) => {
    const data = live();
    if (!data || data.enabled === false || !Array.isArray(data.targets)) return;
    for (const target of data.targets) {
      if (!target || target.kind !== "header" || !target.name) continue;
      event.headers[target.name] = String(target.value ?? "");
    }
  });
}
