//! 分块读取与行边界扫描。
//!
//! 快路径：行完整位于当前块内时零拷贝返回块内切片；
//! 慢路径：跨块长行拼入 `carry` 后返回其切片。超过 `max_line_bytes` 的物理行
//! 强制"虚拟断行"（`truncated` 标记，偏移仍连续覆盖整个文件）。

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::model::file_meta::{FileIdentity, SourceEncoding};

/// 读块大小：1MiB。
const CHUNK_SIZE: usize = 1024 * 1024;

/// 一行物理原文（不含换行符，尾部 `\r` 保留在 `bytes` 中；`content()` 将其去除）。
#[derive(Debug)]
pub struct RawLine<'a> {
    pub bytes: &'a [u8],
    /// 行首的文件绝对字节偏移
    pub start: u64,
    /// 换行符之后的文件绝对字节偏移；EOF 无换行时为 `file_len`
    pub end: u64,
    /// 是否以换行符结尾（文件末行可能没有）
    pub terminated: bool,
    /// 因超过 `max_line_bytes` 被虚拟断行（不是真实换行边界）
    pub truncated: bool,
}

impl RawLine<'_> {
    /// 去除尾部 `\r` 的内容（CRLF 兼容）
    pub fn content(&self) -> &[u8] {
        let mut end = self.bytes.len();
        if end > 0 && self.bytes[end - 1] == b'\r' {
            end -= 1;
        }
        &self.bytes[..end]
    }
}

/// 底层分块读取器：维护"文件绝对偏移 ↔ 块内位置"的对应关系。
struct ChunkReader {
    file: BufReader<File>,
    buf: Box<[u8]>,
    /// `buf[0]` 对应的文件绝对偏移
    buf_base: u64,
    /// `buf` 内有效长度
    filled: usize,
    /// 下一个待消费位置（`buf[pos..filled]` 未消费）
    pos: usize,
    eof: bool,
}

impl ChunkReader {
    fn open(path: &Path) -> std::io::Result<(Self, u64)> {
        let file = File::open(path)?; // 只读共享打开
        let reader = BufReader::with_capacity(CHUNK_SIZE, file);
        let file_len = reader.get_ref().metadata()?.len();
        Ok((
            ChunkReader {
                file: reader,
                // 直接堆分配：debug 构建下 Box::new(大数组) 会先在栈上构造导致爆栈
                buf: vec![0u8; CHUNK_SIZE].into_boxed_slice(),
                buf_base: 0,
                filled: 0,
                pos: 0,
                eof: false,
            },
            file_len,
        ))
    }

    fn abs_pos(&self) -> u64 {
        self.buf_base + self.pos as u64
    }

    /// 读入新块（要求当前块已消费完）。返回是否读到数据。
    fn fill(&mut self) -> std::io::Result<bool> {
        debug_assert!(self.pos >= self.filled);
        if self.eof {
            return Ok(false);
        }
        let mut total = 0;
        // 循环读满（read 可能短读）
        while total < CHUNK_SIZE {
            let n = self.file.read(&mut self.buf[total..])?;
            if n == 0 {
                self.eof = true;
                break;
            }
            total += n;
        }
        self.buf_base += self.filled as u64;
        self.pos = 0;
        self.filled = total;
        Ok(total > 0)
    }
}

/// 行扫描器：逐行产出 `RawLine`，处理跨块行、CRLF、无结尾换行与超长断行。
pub struct LineScanner {
    state: ChunkReader,
    file_len: u64,
    /// UTF-8 BOM 长度（数据起点）
    bom_len: u64,
    /// 跨块长行的拼接缓冲（仅在慢路径使用）
    carry: Vec<u8>,
    max_line_bytes: usize,
    done: bool,
}

impl LineScanner {
    /// 只读打开文件并捕获身份快照与 BOM 探测结果。
    /// 首行 `start` 跳过 UTF-8 BOM（若有）。
    pub fn open(
        path: &Path,
        max_line_bytes: usize,
    ) -> Result<(Self, FileIdentity, SourceEncoding), crate::model::error::CoreError> {
        let (mut state, file_len) = ChunkReader::open(path)?;
        let identity = FileIdentity::capture(state.file.get_ref())?;

        // 探测 BOM：读首 3 字节，再按 BOM 长度回退
        let mut header = [0u8; 3];
        let mut got = 0;
        while got < 3 {
            let n = state.file.read(&mut header[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        let encoding = super::encoding::detect_bom(&header[..got])?;
        let bom_len = match encoding {
            SourceEncoding::Utf8Bom => 3,
            _ => 0,
        };
        // seek 会丢弃 BufReader 内部缓冲，从 BOM 之后重新读块
        state.file.seek(SeekFrom::Start(bom_len as u64))?;
        state.buf_base = bom_len as u64;
        state.pos = 0;
        state.filled = 0;
        state.eof = false;

        Ok((
            LineScanner {
                state,
                file_len,
                bom_len: bom_len as u64,
                carry: Vec::new(),
                max_line_bytes,
                done: false,
            },
            identity,
            encoding,
        ))
    }

    /// 回卷到数据起点（BOM 之后），供采样探测后重新索引。
    pub fn rewind_to_start(&mut self) -> std::io::Result<()> {
        self.state.file.seek(SeekFrom::Start(self.bom_len))?;
        self.state.buf_base = self.bom_len;
        self.state.pos = 0;
        self.state.filled = 0;
        self.state.eof = false;
        self.done = false;
        Ok(())
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// BOM 之后尚未消费的字节数（采样探测用）
    pub fn remaining_bytes(&self) -> u64 {
        self.file_len - self.state.abs_pos()
    }

    /// 产出下一行；`None` 表示 EOF。返回的引用借用 `self`，再次调用前必须消费完。
    pub fn next_line(&mut self) -> std::io::Result<Option<RawLine<'_>>> {
        if self.done {
            return Ok(None);
        }
        let start = self.state.abs_pos();

        // 确保块内有数据（除非 EOF）
        if self.state.pos >= self.state.filled && !self.state.fill()? {
            self.done = true;
            return Ok(None);
        }

        // 快路径：换行符在当前块内 —— 零拷贝
        if let Some(nl) = memchr::memchr(b'\n', &self.state.buf[self.state.pos..self.state.filled])
        {
            let line_end = self.state.pos + nl; // 指向 '\n'
                                                // 超长虚拟断行（快路径同样生效）
            if line_end - self.state.pos > self.max_line_bytes {
                let cut = self.state.pos + self.max_line_bytes;
                let end = self.state.buf_base + cut as u64;
                let bytes = &self.state.buf[self.state.pos..cut];
                self.state.pos = cut;
                return Ok(Some(RawLine {
                    bytes,
                    start,
                    end,
                    terminated: false,
                    truncated: true,
                }));
            }
            let end = self.state.buf_base + (line_end + 1) as u64;
            let bytes = &self.state.buf[self.state.pos..line_end];
            self.state.pos = line_end + 1;
            return Ok(Some(RawLine {
                bytes,
                start,
                end,
                terminated: true,
                truncated: false,
            }));
        }

        // 慢路径：行跨越块边界 —— 拼入 carry
        self.carry.clear();
        self.carry
            .extend_from_slice(&self.state.buf[self.state.pos..self.state.filled]);
        self.state.pos = self.state.filled;

        let mut terminated = false;
        let end;
        loop {
            if !self.state.fill()? {
                // EOF：无结尾换行
                end = start + self.carry.len() as u64;
                self.done = true;
                break;
            }
            if let Some(nl) =
                memchr::memchr(b'\n', &self.state.buf[self.state.pos..self.state.filled])
            {
                let line_end = self.state.pos + nl;
                self.carry
                    .extend_from_slice(&self.state.buf[self.state.pos..line_end]);
                end = self.state.buf_base + (line_end + 1) as u64;
                self.state.pos = line_end + 1;
                terminated = true;
                break;
            }
            self.carry
                .extend_from_slice(&self.state.buf[self.state.pos..self.state.filled]);
            self.state.pos = self.state.filled;

            if self.carry.len() >= self.max_line_bytes {
                // 超长虚拟断行
                self.carry.truncate(self.max_line_bytes);
                end = start + self.max_line_bytes as u64;
                terminated = false;
                let bytes: &[u8] = &self.carry;
                return Ok(Some(RawLine {
                    bytes,
                    start,
                    end,
                    terminated,
                    truncated: true,
                }));
            }
        }

        if self.carry.is_empty() && end == start {
            return Ok(None);
        }
        let bytes: &[u8] = &self.carry;
        let truncated = false;
        Ok(Some(RawLine {
            bytes,
            start,
            end,
            terminated,
            truncated,
        }))
    }

    /// 定位到文件中部下一行边界（detect 采样用）。
    /// 返回 false 表示无法定位（文件过小）。
    pub fn seek_to_middle_line(&mut self) -> std::io::Result<bool> {
        let target = self.file_len / 2;
        // 注意：file_len 含 BOM；target 是绝对偏移。seek 丢弃缓冲。
        if target == 0 {
            return Ok(false);
        }
        self.state.file.seek(SeekFrom::Start(target))?;
        self.state.buf_base = target;
        self.state.pos = 0;
        self.state.filled = 0;
        self.state.eof = false;
        self.done = false;
        // 丢弃半行：读到下一个换行符
        loop {
            if self.state.pos >= self.state.filled && !self.state.fill()? {
                return Ok(false); // 后半段没有换行（单行长文件）
            }
            if let Some(nl) =
                memchr::memchr(b'\n', &self.state.buf[self.state.pos..self.state.filled])
            {
                self.state.pos += nl + 1;
                return Ok(true);
            }
            self.state.pos = self.state.filled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(path: &Path) -> Vec<(Vec<u8>, u64, u64, bool, bool)> {
        let (mut s, _, _) = LineScanner::open(path, 4 * 1024 * 1024).unwrap();
        let mut out = Vec::new();
        while let Some(l) = s.next_line().unwrap() {
            out.push((l.bytes.to_vec(), l.start, l.end, l.terminated, l.truncated));
        }
        out
    }

    #[test]
    fn simple_lf_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, b"a\nbb\nccc").unwrap();
        let lines = scan_all(&p);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], (b"a".to_vec(), 0, 2, true, false));
        assert_eq!(lines[1], (b"bb".to_vec(), 2, 5, true, false));
        assert_eq!(lines[2], (b"ccc".to_vec(), 5, 8, false, false)); // 无结尾换行
    }

    #[test]
    fn crlf_content_strips_cr() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, b"aaa\r\nbbb\r\n").unwrap();
        let (mut s, _, _) = LineScanner::open(&p, 1024).unwrap();
        let l1 = s.next_line().unwrap().unwrap();
        assert_eq!(l1.bytes, b"aaa\r");
        assert_eq!(l1.content(), b"aaa");
        assert_eq!(l1.end, 5);
        let l2 = s.next_line().unwrap().unwrap();
        assert_eq!(l2.end, 10);
        assert!(s.next_line().unwrap().is_none());
    }

    #[test]
    fn bom_first_line_after_bom() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, b"\xEF\xBB\xBFhello\n").unwrap();
        let (mut s, _, enc) = LineScanner::open(&p, 1024).unwrap();
        assert!(matches!(enc, SourceEncoding::Utf8Bom));
        let l = s.next_line().unwrap().unwrap();
        assert_eq!(l.start, 3, "首行 start 应跳过 BOM");
        assert_eq!(l.bytes, b"hello");
        assert_eq!(l.end, 9);
    }

    #[test]
    fn cross_chunk_long_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        // 构造 > 1MiB 的单行，验证跨块拼接
        let mut data = vec![b'x'; 3 * 1024 * 1024];
        data.push(b'\n');
        data.extend_from_slice(b"tail\n");
        std::fs::write(&p, &data).unwrap();
        let lines = scan_all(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0.len(), 3 * 1024 * 1024);
        assert_eq!(lines[0].1, 0);
        assert_eq!(lines[0].2, 3 * 1024 * 1024 + 1);
        assert_eq!(lines[1].0, b"tail".to_vec());
    }

    #[test]
    fn oversized_line_virtual_break() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        let mut data = vec![b'x'; 100];
        data.push(b'\n');
        std::fs::write(&p, &data).unwrap();
        let (mut s, _, _) = LineScanner::open(&p, 10).unwrap(); // 上限 10 字节
        let l = s.next_line().unwrap().unwrap();
        assert!(l.truncated);
        assert_eq!(l.end - l.start, 10);
        // 断行后继续读，偏移最终无缝覆盖全文件
        let mut covered = l.end - l.start;
        while let Some(l) = s.next_line().unwrap() {
            covered += l.end - l.start;
        }
        assert_eq!(covered, 101);
    }

    #[test]
    fn empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, b"").unwrap();
        assert!(scan_all(&p).is_empty());
    }

    #[test]
    fn blank_lines_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, b"a\n\n\nb\n").unwrap();
        let lines = scan_all(&p);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].0, b"");
        assert_eq!(lines[2].0, b"");
        // 无缝覆盖
        assert_eq!(lines[0].2, lines[1].1);
        assert_eq!(lines[1].2, lines[2].1);
        assert_eq!(lines[2].2, lines[3].1);
    }
}
