/**
 * 内置正则预言机：用 JS 正则验证 fixtures 上的头行识别数是否与 Rust 测试预期一致。
 * 用法: node scripts/regex-oracle.mjs
 */
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const fixtures = join(dirname(fileURLToPath(import.meta.url)), "..", "fixtures");

const TS = String.raw`^(?<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?)(?<tz>Z|[+-]\d{2}:?\d{2})?`;
const LV = String.raw`(?<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)`;

const spring = new RegExp(TS + String.raw`\s+` + LV + String.raw`\s+(?:\S+\s+)?---\s+\[(?<thread>[^\]]*)\]\s+(?<logger>\S+)\s*:\s?(?<msg>.*)$`);
const std = new RegExp(TS + String.raw`\s+\[(?<thread>[^\]]*)\]\s+` + LV + String.raw`\s+(?<logger>\S+)\s+-\s?(?<msg>.*)$`);
const nothread = new RegExp(TS + String.raw`\s+` + LV + String.raw`\s+(?<logger>\S+)\s+-\s?(?<msg>.*)$`);

let failures = 0;
function check(file, re, expectHeads) {
  const lines = readFileSync(join(fixtures, file), "utf8")
    .split("\n")
    .map((l) => l.replace(/^﻿/, "").replace(/\r$/, ""))
    .filter((l) => l.trim() !== "");
  let heads = 0;
  const misses = [];
  for (const l of lines) {
    if (re.test(l)) heads++;
    else if (expectHeads !== null) misses.push(l.slice(0, 70));
  }
  const ok = heads === expectHeads;
  if (!ok) failures++;
  console.log(
    file.padEnd(26),
    "lines=" + String(lines.length).padEnd(4),
    "heads=" + String(heads).padEnd(4),
    ok ? "OK" : "MISMATCH want " + expectHeads,
  );
  if (!ok) misses.slice(0, 3).forEach((m) => console.log("   miss:", m));
}

check("basic_app.log", std, 42);
check("multiline_stacktrace.log", std, 5);
check("springboot_default.log", spring, 7);
check("chinese_message.log", std, 10);
check("iso8601_tz.log", std, 6);
check("crlf_windows.log", std, 4);
check("bad_timestamps.log", std, 6);
check("utf8_bom.log", std, 3);
check("no_trailing_newline.log", std, 3);
check("unordered_timestamps.log", std, 6);
check("non_logback.txt", std, 0);
// 负例：std 不应匹配 springboot 行；nothread 不应匹配 basic 行
check("springboot_default.log", std, 0);
check("basic_app.log", nothread, 0);

process.exit(failures ? 1 : 0);
