import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8").replace(/\r\n?/g, "\n");
const config = JSON.parse(read("../src-tauri/tauri.conf.json"));
const lock = JSON.parse(read("../package-lock.json"));
const template = read("../src-tauri/windows/installer.nsi");

test("publisher is explicit without changing the installed application identity", () => {
  assert.equal(config.bundle.publisher, "Lv2dev");
  assert.match(read("../src-tauri/Cargo.toml"), /^authors = \["Lv2dev"\]$/m);
  assert.equal(config.identifier, "com.pointi.agentjuice");
  assert.equal(config.productName, "Juice");
  assert.deepEqual(config.bundle.targets, ["nsis"]);
  assert.equal(config.bundle.windows.nsis.installMode ?? "currentUser", "currentUser");
  assert.match(read("../src-tauri/src/single_instance.rs"), /com\.pointi\.agentjuice\.instance-owner\.v2/);
  assert.deepEqual(config.plugins.updater.endpoints, [
    "https://github.com/Lv2dev/agent-juice/releases/latest/download/latest.json",
  ]);
  assert.equal(config.bundle.windows.nsis.template, "./windows/installer.nsi");
  assert.equal(config.bundle.windows.nsis.installerHooks, "./windows/hooks.nsh");
});

test("installer retains the complete pinned upstream lifecycle except the publisher split", () => {
  assert.equal(lock.packages["node_modules/@tauri-apps/cli"].version, "2.11.4");
  const lines = template.split("\n");
  assert.match(lines[0], /Tauri CLI 2\.11\.4, MIT licensed/);
  assert.match(lines[1], /8909f221d1515955fc843808032bdc5d62209c96/);
  assert.match(lines[2], /legacy registry namespace/);
  const upstream = lines.slice(3).join("\n")
    .replace('!define MANUKEY "Software\\pointi"', '!define MANUKEY "Software\\${MANUFACTURER}"')
    .replace('VIAddVersionKey "CompanyName" "${MANUFACTURER}"\n', "");
  assert.equal(createHash("sha256").update(upstream).digest("hex"),
    "20f4ecc730defb71f1342eaeaec4021df13be3d843abba0effe88ea5835fa079");
  assert.match(template, /!define MANUFACTURER "\{\{manufacturer\}\}"/);
  assert.ok(template.includes('!define MANUKEY "Software\\pointi"'));
  assert.ok(template.includes('VIAddVersionKey "CompanyName" "${MANUFACTURER}"'));
  assert.ok(template.includes('WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "${MANUFACTURER}"'));
  assert.ok(template.includes('!define MANUPRODUCTKEY "${MANUKEY}\\${PRODUCTNAME}"'));
  assert.match(read("../src-tauri/windows/installer.LICENSE"), /Copyright \(c\) 2017 - Present Tauri Apps Contributors/);
});
