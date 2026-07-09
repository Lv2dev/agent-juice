import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const supabasePlanPath = resolve(here, "../.ai/docs/spec/토큰모니터-구현계획-2-Supabase.md");
const supabasePlan = existsSync(supabasePlanPath)
  ? readFileSync(supabasePlanPath, "utf8").replace(/\r\n?/g, "\n")
  : "";

test("G4 Supabase plan pins transport and security boundary gates", (t) => {
  if (!supabasePlan) t.skip("private .ai Supabase plan is unavailable");

  assert.match(supabasePlan, /G4 착수 전 경계 게이트/);
  assert.match(supabasePlan, /대표값 전송 금지/);
  assert.match(supabasePlan, /세션별 `agent_status\.v1`/);
  assert.match(supabasePlan, /`captured_at`.*이벤트 시각/);
  assert.match(supabasePlan, /`updated_at`.*서버 동기화 시각/);
  assert.match(supabasePlan, /2 uid RLS\/Realtime/);
  assert.match(supabasePlan, /service_role.*앱.*금지/);
  assert.match(supabasePlan, /DPAPI|Windows Credential Manager/);
  assert.match(supabasePlan, /로그 레닥션/);
  assert.match(supabasePlan, /프롬프트.*transcript.*금지/);
});
