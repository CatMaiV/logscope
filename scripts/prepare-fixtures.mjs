/**
 * 夹具后处理：Write 工具/编辑器写出的都是 LF + 无 BOM，
 * 这里把需要特殊字节形态的夹具转换为最终形态。
 *
 * 用法: node scripts/prepare-fixtures.mjs
 */
import { readFileSync, writeFileSync, existsSync, statSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dirname, "..");
const fixtures = (...p) => join(root, "fixtures", ...p);

function toCRLF(path) {
  const data = readFileSync(path, "utf8");
  const fixed = data.replace(/\r?\n/g, "\r\n");
  writeFileSync(path, fixed, "utf8");
  console.log(`CRLF   ${path} (${Buffer.byteLength(fixed)} bytes)`);
}

function addBOM(path) {
  const data = readFileSync(path);
  if (data[0] === 0xef && data[1] === 0xbb && data[2] === 0xbf) {
    console.log(`BOM    ${path} (already has BOM)`);
    return;
  }
  writeFileSync(path, Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), data]));
  console.log(`BOM    ${path}`);
}

function assertNoTrailingNewline(path) {
  const data = readFileSync(path);
  if (data.length === 0 || data[data.length - 1] === 0x0a) {
    // 去掉结尾换行
    let end = data.length;
    while (end > 0 && (data[end - 1] === 0x0a || data[end - 1] === 0x0d)) end--;
    writeFileSync(path, data.subarray(0, end));
    console.log(`NO-NL  ${path} (trailing newline stripped)`);
  } else {
    console.log(`NO-NL  ${path} (ok)`);
  }
}

// 大文件夹具：按需生成，不入 git（.gitignore 已排除）
function genLongLine(path, targetBytes = 5 * 1024 * 1024) {
  if (existsSync(path) && statSync(path).size >= targetBytes) {
    console.log(`LONG   ${path} (exists)`);
    return;
  }
  // 单个 ~5MB 超长行 + 正常行收尾
  const parts = ["2026-11-20 09:00:00.000 [main] INFO  c.e.Big - payload="];
  const chunk = "A1b2C3d4E5f6G7h8I9j0".repeat(1000); // 20KB
  let written = parts[0].length;
  const out = [parts[0]];
  while (written < targetBytes) {
    out.push(chunk);
    written += chunk.length;
  }
  out.push("\n2026-11-20 09:00:01.000 [main] INFO  c.e.Big - after long line\n");
  writeFileSync(path, out.join(""));
  console.log(`LONG   ${path} (${statSync(path).size} bytes)`);
}

function genHugeEvent(path, targetBytes = 30 * 1024 * 1024) {
  if (existsSync(path) && statSync(path).size >= targetBytes) {
    console.log(`HUGE   ${path} (exists)`);
    return;
  }
  // 单个 ~30MB 的多行"事件"（无新 head），验证 max_event_bytes 截断
  const out = [];
  out.push("2026-11-20 09:00:00.000 [main] ERROR c.e.Huge - begin massive stack\n");
  const line = "\tat com.example.generated.Frame" + "x".repeat(200) + ".method(Fake.java:1)\n";
  const lineLen = Buffer.byteLength(line);
  let written = 100;
  let i = 0;
  while (written + lineLen < targetBytes) {
    out.push(line);
    written += lineLen;
    i++;
  }
  out.push("2026-11-20 09:00:01.000 [main] INFO  c.e.Huge - after massive stack\n");
  writeFileSync(path, out.join(""));
  console.log(`HUGE   ${path} (${statSync(path).size} bytes, ${i} frames)`);
}

toCRLF(fixtures("crlf_windows.log"));
addBOM(fixtures("utf8_bom.log"));
assertNoTrailingNewline(fixtures("no_trailing_newline.log"));
genLongLine(fixtures("long_line.log"));
genHugeEvent(fixtures("huge_event.log"));
console.log("fixtures ready");
