/**
 * 生成 LogScope 图标源图（512x512 PNG，无第三方依赖）。
 * 设计：深色圆角底 + 左侧时间轴刻度 + 右侧日志级别色条（INFO/WARN/ERROR）。
 * 用法: node scripts/gen-icon.mjs [输出路径]
 */
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

const SIZE = 512;
const out = process.argv[2] ?? "src-tauri/icon-src.png";

// RGBA 像素缓冲
const px = new Uint8Array(SIZE * SIZE * 4);

function set(x, y, r, g, b, a = 255) {
  if (x < 0 || y < 0 || x >= SIZE || y >= SIZE) return;
  const i = (y * SIZE + x) * 4;
  const sa = a / 255;
  const da = px[i + 3] / 255;
  const oa = sa + da * (1 - sa);
  if (oa === 0) return;
  px[i] = Math.round((r * sa + px[i] * da * (1 - sa)) / oa);
  px[i + 1] = Math.round((g * sa + px[i + 1] * da * (1 - sa)) / oa);
  px[i + 2] = Math.round((b * sa + px[i + 2] * da * (1 - sa)) / oa);
  px[i + 3] = Math.round(oa * 255);
}

function rect(x0, y0, w, h, r, g, b, a = 255) {
  for (let y = y0; y < y0 + h; y++) for (let x = x0; x < x0 + w; x++) set(x, y, r, g, b, a);
}

// 圆角遮罩参数
const R = 96;
const bg = [26, 27, 38]; // #1a1b26

for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    // 圆角判断（外圆角 alpha=0）
    const cx = Math.min(Math.max(x, R), SIZE - R);
    const cy = Math.min(Math.max(y, R), SIZE - R);
    const dx = x - cx, dy = y - cy;
    if (dx * dx + dy * dy > R * R) continue;
    set(x, y, bg[0], bg[1], bg[2]);
  }
}

// 左侧时间轴（竖线 + 刻度）
rect(72, 96, 10, 320, 100, 105, 130);
for (let i = 0; i < 5; i++) rect(56, 112 + i * 72, 42, 8, 100, 105, 130);

// 日志级别色条（右侧），错落分布模拟密度时间轴
const bars = [
  // [y, 宽度, 颜色]
  [96, 180, [110, 220, 150]],   // INFO 绿
  [152, 240, [110, 220, 150]],
  [208, 120, [235, 190, 100]],  // WARN 黄
  [264, 220, [110, 220, 150]],
  [320, 260, [240, 110, 100]],  // ERROR 红（高峰）
  [376, 150, [110, 220, 150]],
];
for (const [y, w, c] of bars) {
  rect(160, y, w, 36, c[0], c[1], c[2]);
  rect(160, y, w, 8, Math.min(255, c[0] + 30), Math.min(255, c[1] + 30), Math.min(255, c[2] + 30), 120);
}

// ---------- PNG 编码 ----------
const crcTable = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

// 扫描线（每行前加 filter byte 0）
const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0;
  Buffer.from(px.buffer, y * SIZE * 4, SIZE * 4).copy(raw, y * (SIZE * 4 + 1) + 1);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8;  // bit depth
ihdr[9] = 6;  // color type RGBA
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, png);
console.log(`written ${out} (${png.length} bytes)`);
