//! Mirrors `packages/agent/src/harness/tools/read.ts` — the built-in `read`
//! `AgentTool`. Reads a file (text or image), applies head-truncation with a
//! continuation hint, and detects supported image MIME types. An optional
//! `ReadImageProcessor` seam supports resize/convert implementations; without
//! one, BMP images yield an omitted notice and other formats are inlined.

use std::sync::Arc;

use async_trait::async_trait;
use rpi_agent::agent_tool::AgentTool;
use rpi_agent::error::AgentError;
use rpi_agent::types::{AgentToolResult, TextContentOrImage, ToolResultPartial};
use rpi_ai::types::{
    ConstrainedSamplingConfig, ConstrainedStrictness, ImageContent, ImageContentType, Tool,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::env::ExecutionEnv;
use crate::image::{detect_supported_image_mime_type, encode_base64};
use crate::path_utils::resolve_read_tool_path;
use crate::tools::tool_context::ExecutionToolContext;
use crate::truncate::{
    format_size, TruncationLimit, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};
/// Input for the read tool. Mirrors TS `ReadToolInput`. `offset` is 1-indexed.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct ReadInput {
    /// Path to the file to read (relative or absolute).
    pub path: String,
    /// Line number to start reading from (1-indexed).
    #[serde(default)]
    pub offset: Option<u32>,
    /// Maximum number of lines to read.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Structured details for the read tool. Mirrors TS `ReadToolDetails`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReadToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

// Nuwa 本地补丁(2026-10-21): 流式读取相关常量。旧实现先 `read_binary_file` 全量
// 载入整个文件再 `String::from_utf8_lossy` + `split('\n')` 拆行，limit/offset 只
// 裁剪最终输出——大文件直接 OOM（截断限制根本约束不到读取内存）。

/// 图片魔数嗅探前缀长度。所有支持格式的判定字段都在文件头部（BMP 最多读到
/// offset 28），4KB 前缀足够；对完整字节的复检在载入后按旧语义再做一次。
const IMAGE_SNIFF_BYTES: usize = 4096;

/// 图片输入大小上限。旧实现把任意大的图片全量载入并 base64 内联进上下文；
/// 超过上限直接报错而不是撑爆内存/必被 provider 拒绝。
pub const MAX_IMAGE_READ_BYTES: usize = 20 * 1024 * 1024;

/// 文本流式扫描的分块大小。
const TEXT_SCAN_CHUNK_BYTES: usize = 256 * 1024;

/// 单行内容累积上限：超过 `DEFAULT_MAX_BYTES` 的行永远进不了保留窗口
/// （字节预算必然放不下），只继续数长度（供 first_line_exceeds 汇报）。
const LINE_ACCUM_CAP: usize = DEFAULT_MAX_BYTES + 1;

/// 保留窗口行数上限：`truncate_head` 至多输出 `DEFAULT_MAX_LINES` 行，多保留
/// 一行用于覆盖"文件以 \n 结尾时窗口末尾的空行"参与 join 的情形。
const RETAIN_CAP: usize = DEFAULT_MAX_LINES + 1;

/// `TextWindowScan::finish` 的产物：read 工具文本路径所需的全部信息。
struct TextWindow {
    /// 等价于旧实现 `text.split('\n').collect::<Vec<_>>().len()`（含末尾空元素）。
    total_file_lines: usize,
    /// 窗口首行的字节长度（旧实现 `all_lines[start_line].as_bytes().len()`；
    /// 对超过 `LINE_ACCUM_CAP` 的超长行报告原始字节长度——旧实现报 lossy 解码后
    /// 长度，二者仅在">50KB 且含非法 UTF-8 的单行"这一病态边缘有差异）。
    first_selected_line_len: usize,
    /// 与旧路径 `truncate_head(selected_content, TruncationOptions::default())`
    /// 逐字段一致的结果（由 read.rs 测试里的性质测试钉死）。
    truncation: TruncationResult,
}

/// Nuwa 本地补丁(2026-10-21): 流式窗口扫描器。按块喂入文件字节，逐行处理：
/// 所有行都参与计数（文件总行数、窗口行数/字节数、首行长度——输出提示
/// "of {total} lines" / "[N more lines...]" 需要全文件行数，因此文件仍要被
/// 完整扫描一遍，修的是内存而不是 I/O），但只有可能进入最终输出的内容才被
/// 保留（≤ RETAIN_CAP 行且 ≤ DEFAULT_MAX_BYTES 字节），内存有界。
struct TextWindowScan {
    start_line: usize,
    limit: Option<u32>,
    /// 已完整经过的 '\n' 数；total_file_lines = newline_count + 1。
    newline_count: usize,
    /// 当前未结束行的累计字节数（不含 '\n'）。
    line_len: usize,
    /// 当前行内容——仅当该行可能进入保留窗口时累积；超过 LINE_ACCUM_CAP
    /// 置 None（只数长度）。
    line_buf: Option<Vec<u8>>,
    /// 窗口内原始行数（含末尾空行，等价 end_line - start_line）。
    w_raw: usize,
    /// 窗口内各行字节数之和（不含分隔符）。
    sum_lens: usize,
    first_line_len: usize,
    last_line_len: usize,
    /// 保留的窗口行（lossy 解码后），至多 RETAIN_CAP 行。
    retained: Vec<String>,
    /// 保留内容的 join("\n") 字节数（等价 truncate_head 的 output_bytes_count）。
    retained_bytes: usize,
    /// 首个放不进字节预算的窗口行下标（等价 truncate_head 循环 break 的位置）。
    overflow_at: Option<usize>,
}

impl TextWindowScan {
    fn new(start_line: usize, limit: Option<u32>) -> Self {
        let mut scan = Self {
            start_line,
            limit,
            newline_count: 0,
            line_len: 0,
            line_buf: None,
            w_raw: 0,
            sum_lens: 0,
            first_line_len: 0,
            last_line_len: 0,
            retained: Vec::new(),
            retained_bytes: 0,
            overflow_at: None,
        };
        // 第 0 行从调用方喂入第一块前就开始累积判定。
        scan.line_buf = scan.candidate_buf(0);
        scan
    }

    fn in_window(&self, idx: usize) -> bool {
        idx >= self.start_line
            && self
                .limit
                .is_none_or(|l| idx < self.start_line + l as usize)
    }

    /// 该行是否需要临时累积内容：窗口内的行都要（长度计量必须按 lossy 解码
    /// 后的字节数，与旧实现"整文件 lossy 后 split"逐字节一致，含非法 UTF-8 的
    /// U+FFFD 展开），`start_line` 行即使 limit=0 也要（first_line_exceeds 的
    /// 尺寸汇报用它）。累积只是逐行临时缓冲（≤ LINE_ACCUM_CAP），是否长期保留
    /// 由 `process_line` 的预算判定决定，内存仍然有界。
    fn candidate_buf(&self, idx: usize) -> Option<Vec<u8>> {
        if self.in_window(idx) || idx == self.start_line {
            Some(Vec::new())
        } else {
            None
        }
    }

    fn feed_segment(&mut self, segment: &[u8]) {
        self.line_len += segment.len();
        if let Some(buf) = self.line_buf.as_mut() {
            if buf.len() + segment.len() <= LINE_ACCUM_CAP {
                buf.extend_from_slice(segment);
            } else {
                // 超长行：放弃内容，只保留计数（长度通过 line_len 跟踪）。
                self.line_buf = None;
            }
        }
    }

    fn finish_line(&mut self) {
        let idx = self.newline_count;
        let len = self.line_len;
        let content = self.line_buf.take();
        self.process_line(idx, len, content);
        self.newline_count += 1;
        self.line_len = 0;
        self.line_buf = self.candidate_buf(self.newline_count);
    }

    /// 喂入一个字节块；块边界可以落在任意位置（行内容跨块累积）。
    fn push_chunk(&mut self, chunk: &[u8]) {
        let mut rest = chunk;
        loop {
            match rest.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    self.feed_segment(&rest[..i]);
                    self.finish_line();
                    rest = &rest[i + 1..];
                }
                None => {
                    self.feed_segment(rest);
                    break;
                }
            }
        }
    }

    fn process_line(&mut self, idx: usize, raw_len: usize, content: Option<Vec<u8>>) {
        // 保留了内容的行按 lossy 解码后的长度计量——与旧实现"整文件 lossy 后
        // split"逐字节一致（含非法 UTF-8 时 U+FFFD 的 3 字节展开）；超长行
        // （content 已放弃，raw_len > LINE_ACCUM_CAP）按原始字节数近似，其
        // lossy 长度必然同样超出字节预算，判定结果一致，仅 details 里的
        // total_bytes 数值在"非法 UTF-8 + >50KB 单行"的病态组合下有差异。
        // Cow：合法 UTF-8（常态）零拷贝，只有非法行才分配替换串。
        let decoded = content.as_deref().map(String::from_utf8_lossy);
        let len = decoded.as_ref().map(|c| c.len()).unwrap_or(raw_len);
        // 旧实现的 first_line_size 取 all_lines[start_line]，与 limit 无关
        // （limit=0 时窗口为空也要能量到该行）。
        if idx == self.start_line {
            self.first_line_len = len;
        }
        if !self.in_window(idx) {
            return;
        }
        if self.w_raw == 0 {
            debug_assert_eq!(self.first_line_len, len);
        }
        self.w_raw += 1;
        self.sum_lens += len;
        self.last_line_len = len;
        if self.overflow_at.is_some() || self.retained.len() >= RETAIN_CAP {
            return;
        }
        let sep = if self.retained.is_empty() { 0 } else { 1 };
        // 与 truncate_head 的 `output_bytes_count + line_bytes > max_bytes` 一致。
        if self.retained_bytes + sep + len <= DEFAULT_MAX_BYTES {
            if let Some(text) = decoded {
                self.retained_bytes += sep + len;
                self.retained.push(text.into_owned());
            } else {
                // 不可达的防御分支：content 为 None ⟹ raw_len > DEFAULT_MAX_BYTES
                // ⟹ 字节预算必然放不下，应走 else。
                self.overflow_at = Some(self.w_raw - 1);
            }
        } else {
            self.overflow_at = Some(self.w_raw - 1);
        }
    }

    /// EOF：冲刷末尾行并组装 `TextWindow`（语义与旧实现的
    /// split/truncate_head 管线逐字段一致）。
    fn finish(mut self) -> TextWindow {
        // 文件总是以"最后一行"结束（可能为空）——恰好对应 split('\n') 在末尾
        // '\n' 之后仍产生一个元素的行为。
        {
            let idx = self.newline_count;
            let len = self.line_len;
            let content = self.line_buf.take();
            self.process_line(idx, len, content);
            self.newline_count += 1;
        }
        let total_file_lines = self.newline_count; // = '\n' 数 + 1
        let w_raw = self.w_raw;
        // selected = 窗口行 join("\n")：字节数 = 行长之和 + (行数-1) 个分隔符。
        let total_bytes = if w_raw == 0 {
            0
        } else {
            self.sum_lens + w_raw - 1
        };
        // split_lines_for_counting(selected)：内容以 '\n' 结尾时丢掉末尾空元素。
        // join 结果以 '\n' 结尾 ⟺ 窗口 ≥2 行且末行为空；窗口只有 1 个空行时
        // selected 为 ""，计数同样是 0。
        let drop_last = w_raw >= 2 && self.last_line_len == 0;
        let w_prime = if w_raw == 0 || (w_raw == 1 && self.first_line_len == 0) {
            0
        } else if drop_last {
            w_raw - 1
        } else {
            w_raw
        };
        let max_lines = DEFAULT_MAX_LINES;
        let max_bytes = DEFAULT_MAX_BYTES;
        let truncated = w_prime > max_lines || total_bytes > max_bytes;
        let truncation = if !truncated {
            // 未截断：窗口全部行必然都在保留集里（行数 ≤ RETAIN_CAP 且累计
            // 字节 ≤ total_bytes ≤ max_bytes），content 就是 selected 原文。
            TruncationResult {
                content: self.retained.join("\n"),
                truncated: false,
                truncated_by: None,
                total_lines: w_prime,
                total_bytes,
                output_lines: w_prime,
                output_bytes: total_bytes,
                last_line_partial: false,
                first_line_exceeds_limit: false,
                max_lines,
                max_bytes,
            }
        } else if w_prime >= 1 && self.first_line_len > max_bytes {
            // truncate_head 的 first-line 检查：单行超预算 → 空输出 + 专用提示。
            TruncationResult {
                content: String::new(),
                truncated: true,
                truncated_by: Some(TruncationLimit::Bytes),
                total_lines: w_prime,
                total_bytes,
                output_lines: 0,
                output_bytes: 0,
                last_line_partial: false,
                first_line_exceeds_limit: true,
                max_lines,
                max_bytes,
            }
        } else {
            let upper = w_prime.min(max_lines);
            let (k, truncated_by) = match self.overflow_at {
                // 溢出发生在 truncate_head 循环覆盖范围内 → 按字节截断。
                Some(j) if j < upper => (j, Some(TruncationLimit::Bytes)),
                // 溢出只可能落在被丢弃的末尾空行/循环上界之外（如恰好 50KB 的
                // 边界情形）→ 走 truncate_head 的循环完成路径 + 行数修正。
                _ => {
                    let bytes_k = joined_len(&self.retained, upper);
                    let by = if upper >= max_lines && bytes_k <= max_bytes {
                        Some(TruncationLimit::Lines)
                    } else {
                        None
                    };
                    (upper, by)
                }
            };
            let content = self.retained[..k.min(self.retained.len())].join("\n");
            let output_bytes = content.len();
            TruncationResult {
                content,
                truncated: true,
                truncated_by,
                total_lines: w_prime,
                total_bytes,
                output_lines: k,
                output_bytes,
                last_line_partial: false,
                first_line_exceeds_limit: false,
                max_lines,
                max_bytes,
            }
        };
        TextWindow {
            total_file_lines,
            first_selected_line_len: self.first_line_len,
            truncation,
        }
    }
}

/// `lines[..k]` join("\n") 后的字节数（k 截断到实际保留数）。
fn joined_len(lines: &[String], k: usize) -> usize {
    let take = k.min(lines.len());
    if take == 0 {
        return 0;
    }
    lines[..take].iter().map(|l| l.len()).sum::<usize>() + take - 1
}

/// Options for `create_read_tool`. Mirrors TS `ReadToolOptions`.
pub struct ReadToolOptions {
    /// Whether an injected image processor should resize images. Default true.
    pub auto_resize_images: bool,
    /// Optional image conversion/resizing implementation.
    pub image_processor: Option<Arc<dyn ReadImageProcessor>>,
}

impl Clone for ReadToolOptions {
    fn clone(&self) -> Self {
        Self {
            auto_resize_images: self.auto_resize_images,
            image_processor: self.image_processor.clone(),
        }
    }
}

impl std::fmt::Debug for ReadToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadToolOptions")
            .field("auto_resize_images", &self.auto_resize_images)
            .field("image_processor", &self.image_processor.is_some())
            .finish()
    }
}

impl Default for ReadToolOptions {
    fn default() -> Self {
        Self {
            auto_resize_images: true,
            image_processor: None,
        }
    }
}

/// Optional image-conversion seam. Mirrors TS `ReadImageProcessor`.
pub trait ReadImageProcessor: Send + Sync {
    fn process(
        &self,
        bytes: &[u8],
        mime_type: &str,
        auto_resize: bool,
    ) -> Result<ProcessedImage, String>;
}

/// Result of a successful [`ReadImageProcessor::process`].
pub struct ProcessedImage {
    pub data: String,
    pub mime_type: String,
    pub hints: Vec<String>,
}

/// The built-in `read` tool. Holds an `Arc<dyn ExecutionEnv>` (read-only view)
/// and an optional image processor.
pub struct ReadTool {
    schema: Tool,
    env: Arc<dyn ExecutionEnv>,
    options: ReadToolOptions,
    image_processor: Option<Arc<dyn ReadImageProcessor>>,
}

impl ReadTool {
    fn schema() -> Tool {
        let params = schemars::schema_for!(ReadInput);
        Tool {
            name: "read".to_string(),
            description: format!(
                "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). \
                 Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} \
                 lines or {kb}KB (whichever is hit first). Use offset/limit for large files. When you need \
                 the full file, continue with offset until complete.",
                kb = DEFAULT_MAX_BYTES / 1024
            ),
            parameters: rpi_ai::types::Schema::new(serde_json::to_value(params).unwrap_or_default()),
            constrained_sampling: Some(ConstrainedSamplingConfig::JsonSchema { strict: ConstrainedStrictness::Prefer }),
        }
    }
}

/// Build the `read` tool from a context. Mirrors TS `createReadTool`.
pub fn create_read_tool(
    context: &ExecutionToolContext,
    options: Option<ReadToolOptions>,
) -> Arc<dyn AgentTool> {
    let options = options.unwrap_or_default();
    let image_processor = options.image_processor.clone();
    Arc::new(ReadTool {
        schema: ReadTool::schema(),
        env: context.env().clone(),
        options,
        image_processor,
    })
}

#[async_trait]
impl AgentTool for ReadTool {
    fn schema(&self) -> &Tool {
        &self.schema
    }
    fn label(&self) -> &str {
        "read"
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: serde_json::Value,
        signal: CancellationToken,
        _on_update: Arc<dyn Fn(ToolResultPartial) + Send + Sync>,
    ) -> Result<AgentToolResult, AgentError> {
        let input: ReadInput = serde_json::from_value(params)
            .map_err(|e| AgentError::Validation(format!("read input invalid: {e}")))?;

        let abs = resolve_read_tool_path(&*self.env, &input.path, Some(&signal))
            .await
            .map_err(file_err_to_agent)?;
        // Nuwa 本地补丁(2026-10-21): 不再无条件 `read_binary_file` 全量载入。
        // 先用 4KB 前缀嗅探图片魔数；图片按 MAX_IMAGE_READ_BYTES 上限整读，
        // 文本走 TextWindowScan 分块流式扫描（内存有界，返回语义不变）。
        let head = self
            .env
            .read_binary_file_range(&abs, 0, IMAGE_SNIFF_BYTES, Some(&signal))
            .await
            .map_err(file_err_to_agent)?;

        // Image path.
        if detect_supported_image_mime_type(&head).is_some() {
            let bytes = self
                .env
                .read_binary_file_range(&abs, 0, MAX_IMAGE_READ_BYTES + 1, Some(&signal))
                .await
                .map_err(file_err_to_agent)?;
            // Nuwa 本地补丁(2026-10-21): 图片输入大小上限——旧实现会把任意大的
            // 图片全量载入并 base64 内联（OOM 风险 + provider 必拒）。
            if bytes.len() > MAX_IMAGE_READ_BYTES {
                return Err(AgentError::Tool(format!(
                    "Image file {} exceeds the {} read limit",
                    input.path,
                    format_size(MAX_IMAGE_READ_BYTES)
                )));
            }
            // 用完整字节复检：前缀嗅探对动画 PNG 等情形可能误报，而旧语义以
            // 全量字节的检测结果为准（复检为 None 时与旧实现一样落入文本路径）。
            if let Some(mime) = detect_supported_image_mime_type(&bytes) {
                return Ok(image_result(
                    mime,
                    &bytes,
                    &self.options,
                    self.image_processor.as_deref(),
                ));
            }
        }

        // Text path — streaming window scan (bounded memory).
        let start_line = input
            .offset
            .map(|o| (o.saturating_sub(1)) as usize)
            .unwrap_or(0);
        let mut scan = TextWindowScan::new(start_line, input.limit);
        let mut read_offset = 0u64;
        loop {
            let chunk = self
                .env
                .read_binary_file_range(&abs, read_offset, TEXT_SCAN_CHUNK_BYTES, Some(&signal))
                .await
                .map_err(file_err_to_agent)?;
            let got = chunk.len();
            scan.push_chunk(&chunk);
            if got < TEXT_SCAN_CHUNK_BYTES {
                break;
            }
            read_offset += got as u64;
        }
        let window = scan.finish();

        // Mirror TS: `textContent.split("\n")` keeps a trailing "" when the
        // content ends with '\n'; the line-count + slice math below uses the raw
        // split (NOT `split_lines_for_counting`), so we keep it as-is.
        let total_file_lines = window.total_file_lines;
        let start_line_display = start_line + 1;
        if start_line >= total_file_lines {
            return Err(AgentError::Tool(format!(
                "Offset {} is beyond end of file ({} lines total)",
                input.offset.unwrap_or(1),
                total_file_lines
            )));
        }

        // 等价旧实现：limit 时 user_limited_lines = Some(end_line - start_line)。
        let user_limited_lines: Option<usize> = input.limit.map(|limit| {
            let end_line = std::cmp::min(start_line + limit as usize, total_file_lines);
            end_line - start_line
        });

        let truncation = window.truncation;
        let (output_text, details): (String, Option<TruncationResult>) = if truncation
            .first_line_exceeds_limit
        {
            let first_line_size = format_size(window.first_selected_line_len);
            (
                    format!(
                        "[Line {start_line_display} is {first_line_size}, exceeds {} limit. \
                         Use bash: sed -n '{start_line_display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
                        format_size(DEFAULT_MAX_BYTES),
                        input.path
                    ),
                    Some(truncation),
                )
        } else if truncation.truncated {
            let end_line_display = start_line_display + truncation.output_lines - 1;
            let next_offset = end_line_display + 1;
            let mut text = truncation.content.clone();
            match truncation.truncated_by {
                Some(crate::truncate::TruncationLimit::Lines) => {
                    text.push_str(&format!(
                        "\n\n[Showing lines {start_line_display}-{end_line_display} of \
                             {total_file_lines}. Use offset={next_offset} to continue.]"
                    ));
                }
                _ => {
                    text.push_str(&format!(
                        "\n\n[Showing lines {start_line_display}-{end_line_display} of \
                             {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                        format_size(DEFAULT_MAX_BYTES)
                    ));
                }
            }
            (text, Some(truncation))
        } else if let Some(ull) = user_limited_lines {
            // Nuwa 本地补丁(2026-10-21): all_lines.len() → window.total_file_lines
            // （流式扫描不再物化全文件行数组，计数语义不变）。
            if start_line + ull < total_file_lines {
                let remaining = total_file_lines - (start_line + ull);
                let next_offset = start_line + ull + 1;
                (
                        format!(
                            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                            truncation.content
                        ),
                        None,
                    )
            } else {
                (truncation.content, None)
            }
        } else {
            (truncation.content, None)
        };

        let details_value = match details {
            Some(t) => serde_json::to_value(ReadToolDetails {
                truncation: Some(t),
            })
            .unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };
        Ok(AgentToolResult {
            content: vec![TextContentOrImage::text(output_text)],
            details: details_value,
            ..Default::default()
        })
    }
}

/// Build the image-path result, honoring an optional processor. Mirrors the TS
/// image branch.
fn image_result(
    mime: &str,
    bytes: &[u8],
    options: &ReadToolOptions,
    processor: Option<&dyn ReadImageProcessor>,
) -> AgentToolResult {
    if let Some(proc) = processor {
        match proc.process(bytes, mime, options.auto_resize_images) {
            Ok(p) => {
                let hints = if p.hints.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", p.hints.join("\n"))
                };
                return AgentToolResult {
                    content: vec![
                        TextContentOrImage::text(format!(
                            "Read image file [{}]{}",
                            p.mime_type, hints
                        )),
                        TextContentOrImage::Image(ImageContent {
                            kind: ImageContentType,
                            data: p.data,
                            mime_type: p.mime_type,
                        }),
                    ],
                    details: serde_json::Value::Null,
                    ..Default::default()
                };
            }
            Err(message) => {
                return AgentToolResult {
                    content: vec![TextContentOrImage::text(format!(
                        "Read image file [{mime}]\n{message}"
                    ))],
                    details: serde_json::Value::Null,
                    ..Default::default()
                };
            }
        }
    }
    // No processor — BMP is omitted; others are inlined.
    if mime == "image/bmp" {
        return AgentToolResult {
            content: vec![TextContentOrImage::text(
                "Read image file [image/bmp]\n[Image omitted: configure an imageProcessor to convert BMP images.]",
            )],
            details: serde_json::Value::Null,
            ..Default::default()
        };
    }
    AgentToolResult {
        content: vec![
            TextContentOrImage::text(format!("Read image file [{mime}]")),
            TextContentOrImage::Image(ImageContent {
                kind: ImageContentType,
                data: encode_base64(bytes),
                mime_type: mime.to_string(),
            }),
        ],
        details: serde_json::Value::Null,
        ..Default::default()
    }
}

/// Map a `FileError` into an `AgentError::Tool`, mirroring the TS `getOrThrow`
/// surface (the message carries the path + code).
fn file_err_to_agent(e: crate::error::FileError) -> AgentError {
    AgentError::Tool(e.to_string())
}

// Nuwa 本地补丁(2026-10-21): 流式窗口扫描的回归测试。核心是"性质测试"——把旧
// 实现（全量载入 → lossy → split → 窗口切片 → truncate_head）原样复刻为
// `legacy_window`，随机内容 + 随机 offset/limit + 随机分块大小下与流式扫描
// 逐字段比对，钉死"返回内容语义不变"；另有内存有界断言、真实大文件（Os 环境）
// 与图片上限测试。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FileSystem;
    use crate::truncate::{truncate_head, TruncationOptions};

    /// 旧实现管线的完整复刻（仅测试用），作为流式扫描的对照基准。
    fn legacy_window(bytes: &[u8], start_line: usize, limit: Option<u32>) -> TextWindow {
        let text = String::from_utf8_lossy(bytes).into_owned();
        let all_lines: Vec<&str> = text.split('\n').collect();
        let total_file_lines = all_lines.len();
        let first_selected_line_len = if start_line < all_lines.len() {
            all_lines[start_line].len()
        } else {
            0
        };
        let selected = if start_line >= all_lines.len() {
            String::new()
        } else if let Some(limit) = limit {
            let end = std::cmp::min(start_line + limit as usize, all_lines.len());
            all_lines[start_line..end].join("\n")
        } else {
            all_lines[start_line..].join("\n")
        };
        TextWindow {
            total_file_lines,
            first_selected_line_len,
            truncation: truncate_head(&selected, TruncationOptions::default()),
        }
    }

    fn scan_window(
        bytes: &[u8],
        start_line: usize,
        limit: Option<u32>,
        chunk: usize,
    ) -> TextWindow {
        let mut scan = TextWindowScan::new(start_line, limit);
        for c in bytes.chunks(chunk.max(1)) {
            scan.push_chunk(c);
        }
        scan.finish()
    }

    fn assert_windows_eq(got: &TextWindow, want: &TextWindow, ctx: &str) {
        assert_eq!(got.total_file_lines, want.total_file_lines, "{ctx}: total_file_lines");
        assert_eq!(
            got.first_selected_line_len, want.first_selected_line_len,
            "{ctx}: first_selected_line_len"
        );
        let (g, w) = (&got.truncation, &want.truncation);
        assert_eq!(g.content, w.content, "{ctx}: content");
        assert_eq!(g.truncated, w.truncated, "{ctx}: truncated");
        assert_eq!(g.truncated_by, w.truncated_by, "{ctx}: truncated_by");
        assert_eq!(g.total_lines, w.total_lines, "{ctx}: total_lines");
        assert_eq!(g.total_bytes, w.total_bytes, "{ctx}: total_bytes");
        assert_eq!(g.output_lines, w.output_lines, "{ctx}: output_lines");
        assert_eq!(g.output_bytes, w.output_bytes, "{ctx}: output_bytes");
        assert_eq!(g.last_line_partial, w.last_line_partial, "{ctx}: last_line_partial");
        assert_eq!(
            g.first_line_exceeds_limit, w.first_line_exceeds_limit,
            "{ctx}: first_line_exceeds_limit"
        );
        assert_eq!(g.max_lines, w.max_lines, "{ctx}: max_lines");
        assert_eq!(g.max_bytes, w.max_bytes, "{ctx}: max_bytes");
    }

    /// 固定边界样本：空文件、仅换行、末尾换行、恰好 2000/2001 行、恰好 50KB、
    /// 50KB+1、超长单行、limit=0、offset 越过 EOF、窗口截在空行处等。
    #[test]
    fn scan_matches_legacy_on_fixed_edge_cases() {
        let mut cases: Vec<(String, Vec<u8>)> = vec![
            ("empty".into(), Vec::new()),
            ("only-newline".into(), b"\n".to_vec()),
            ("two-newlines".into(), b"\n\n".to_vec()),
            ("no-trailing-nl".into(), b"a\nb".to_vec()),
            ("trailing-nl".into(), b"a\nb\n".to_vec()),
            ("blank-lines".into(), b"a\n\n\nb\n\n".to_vec()),
            (
                "line-exactly-max-bytes".into(),
                format!("{}\n", "a".repeat(DEFAULT_MAX_BYTES)).into_bytes(),
            ),
            (
                "line-max-bytes-plus-one".into(),
                format!("{}\n", "a".repeat(DEFAULT_MAX_BYTES + 1)).into_bytes(),
            ),
            (
                // 恰好触发 truncated_by=None 的边界：前两行 join 正好 50KB，
                // 末尾空行使 total_bytes = 50KB+1。
                "exact-50kb-join-plus-trailing-empty".into(),
                format!("{}\n{}\n", "a".repeat(25599), "b".repeat(25600)).into_bytes(),
            ),
            (
                "oversized-first-line-then-small".into(),
                format!("{}\nsmall\n", "x".repeat(DEFAULT_MAX_BYTES + 100)).into_bytes(),
            ),
            (
                "invalid-utf8-short-lines".into(),
                vec![b'a', 0xff, b'b', b'\n', b'c', 0xc0, 0x80, b'd', b'\n'],
            ),
            (
                "cr-lf-endings".into(),
                b"line1\r\nline2\r\nline3\r\n".to_vec(),
            ),
        ];
        // 2000 / 2001 / 3000 行（带末尾换行）。
        for n in [1999usize, 2000, 2001, 3000] {
            let body: String = (0..n).map(|i| format!("l{i}\n")).collect();
            cases.push((format!("{n}-lines-trailing-nl"), body.into_bytes()));
            let body_no_nl: String = (0..n)
                .map(|i| format!("l{i}"))
                .collect::<Vec<_>>()
                .join("\n");
            cases.push((format!("{n}-lines-no-trailing-nl"), body_no_nl.into_bytes()));
        }
        // 超过 50KB 的多行内容（按字节截断）。
        let many_long: String = (0..60).map(|i| format!("line-{i:04}-{}\n", "y".repeat(1000))).collect();
        cases.push(("60x1kb-lines".into(), many_long.into_bytes()));

        let limits: Vec<Option<u32>> = vec![None, Some(0), Some(1), Some(2), Some(50), Some(1999), Some(2000), Some(5000)];
        for (name, bytes) in &cases {
            let line_count = bytes.iter().filter(|b| **b == b'\n').count() + 1;
            let starts = [0usize, 1, 2, line_count.saturating_sub(1), line_count, line_count + 3];
            for start in starts {
                for limit in &limits {
                    for chunk in [1usize, 3, 997, 64 * 1024, usize::MAX] {
                        let ctx = format!("{name} start={start} limit={limit:?} chunk={chunk}");
                        let want = legacy_window(bytes, start, *limit);
                        let got = scan_window(bytes, start, *limit, chunk);
                        assert_windows_eq(&got, &want, &ctx);
                    }
                }
            }
        }
    }

    /// 简易 LCG（rpi-tools 无 rand 依赖）。
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 11
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % (n as u64)) as usize
        }
    }

    #[test]
    fn scan_matches_legacy_on_randomized_content() {
        let mut rng = Lcg(0x5eed_2026_1021);
        for case in 0..150 {
            // 随机行集合：多数短行，少量 1KB 行，偶尔一条超长 ASCII 行。
            let mut bytes: Vec<u8> = Vec::new();
            let n_lines = rng.below(120);
            for _ in 0..n_lines {
                let kind = rng.below(100);
                let line: Vec<u8> = if kind < 70 {
                    // 短行，可能含非法 UTF-8（lossy 长度参与预算计算）。
                    let len = rng.below(40);
                    (0..len)
                        .map(|_| {
                            if rng.below(20) == 0 {
                                [0xffu8, 0xc0, 0x80][rng.below(3)]
                            } else {
                                b'a' + rng.below(26) as u8
                            }
                        })
                        .collect()
                } else if kind < 95 {
                    vec![b'k'; rng.below(2000)]
                } else {
                    // 超长行（纯 ASCII：raw == lossy，避免已记录的病态边缘）。
                    vec![b'z'; DEFAULT_MAX_BYTES + 1 + rng.below(4096)]
                };
                bytes.extend_from_slice(&line);
                bytes.push(b'\n');
            }
            // 随机去掉末尾换行。
            if rng.below(2) == 0 && bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            let line_count = bytes.iter().filter(|b| **b == b'\n').count() + 1;
            let start = rng.below(line_count + 2);
            let limit = match rng.below(4) {
                0 => None,
                1 => Some(0),
                2 => Some(rng.below(30) as u32),
                _ => Some(rng.below(2500) as u32 + 1),
            };
            for chunk in [1usize, 7, 4096, usize::MAX] {
                let ctx = format!("case{case} start={start} limit={limit:?} chunk={chunk}");
                let want = legacy_window(&bytes, start, limit);
                let got = scan_window(&bytes, start, limit, chunk);
                assert_windows_eq(&got, &want, &ctx);
            }
        }
    }

    /// 内存有界：保留集永远不超过 RETAIN_CAP 行 / DEFAULT_MAX_BYTES 字节，
    /// 与喂入的文件大小无关（这正是被修的 OOM 缺陷）。
    #[test]
    fn scan_retention_is_bounded_regardless_of_input_size() {
        // 20 万行 × ~13 字节 ≈ 2.6MB 合成内容，分块喂入。
        let mut scan = TextWindowScan::new(0, None);
        for i in 0..200_000usize {
            let line = format!("line-{i:07}\n");
            scan.push_chunk(line.as_bytes());
        }
        assert!(scan.retained.len() <= RETAIN_CAP);
        assert!(scan.retained_bytes <= DEFAULT_MAX_BYTES);
        let window = scan.finish();
        assert_eq!(window.total_file_lines, 200_001);
        assert!(window.truncation.truncated);
        assert_eq!(
            window.truncation.truncated_by,
            Some(TruncationLimit::Lines)
        );
        assert_eq!(window.truncation.output_lines, DEFAULT_MAX_LINES);
    }

    /// limit=1 时保留集只有首行——大文件也不会读入远超所需的内存。
    #[test]
    fn scan_with_limit_one_retains_only_the_first_line() {
        let mut scan = TextWindowScan::new(0, Some(1));
        for i in 0..50_000usize {
            scan.push_chunk(format!("row-{i:06}-padding\n").as_bytes());
        }
        assert_eq!(scan.retained.len(), 1);
        let window = scan.finish();
        assert_eq!(window.truncation.content, "row-000000-padding");
        assert!(!window.truncation.truncated);
        assert_eq!(window.total_file_lines, 50_001);
    }

    // ---- 工具级集成：真实 OsExecutionEnv + 大临时文件 -----------------------

    fn tool_text(r: &rpi_agent::types::AgentToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| match c {
                TextContentOrImage::Text(t) => Some(t.text.clone()),
                TextContentOrImage::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn read_tool_large_file_limit_one_returns_first_line_via_os_env() {
        use crate::file_mutation_queue::MutatingEnv;
        use crate::os_env::OsExecutionEnv;
        use crate::ExecutionToolContext;

        // 真实临时文件：15 万行（约 3MB），远超 50KB 输出预算。
        let tmp = std::env::temp_dir().join(format!("rpi-read-large-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("temp dir");
        let path = tmp.join("big.txt");
        {
            use std::io::Write;
            let mut f = std::io::BufWriter::new(std::fs::File::create(&path).expect("create"));
            for i in 0..150_000u32 {
                writeln!(f, "line-{i:06}-payload").expect("write line");
            }
        }

        let env = Arc::new(OsExecutionEnv::with_cwd(tmp.clone()));
        let env_dyn: Arc<dyn ExecutionEnv> = env.clone();
        let mut_env: Arc<dyn MutatingEnv> = env.clone();
        let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));
        let tool = create_read_tool(&ctx, None);

        let result = tool
            .execute(
                "read-large",
                serde_json::json!({ "path": "big.txt", "limit": 1 }),
                CancellationToken::new(),
                Arc::new(|_p| ()),
            )
            .await
            .expect("large file read with limit=1 must succeed");
        let out = tool_text(&result);
        assert!(
            out.starts_with("line-000000-payload"),
            "first line must be exact, got: {out}"
        );
        assert!(
            out.contains("[150000 more lines in file. Use offset=2 to continue.]"),
            "continuation notice must carry the exact total, got: {out}"
        );

        // 同一文件 offset 越过 EOF 的报错语义也保持不变。
        let err = tool
            .execute(
                "read-large",
                serde_json::json!({ "path": "big.txt", "offset": 99999999 }),
                CancellationToken::new(),
                Arc::new(|_p| ()),
            )
            .await
            .expect_err("offset beyond EOF must fail");
        match err {
            AgentError::Tool(msg) => {
                assert!(msg.contains("beyond end of file"), "{msg}");
                assert!(msg.contains("150001 lines total"), "{msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 图片输入大小上限：超过 MAX_IMAGE_READ_BYTES 的图片必须报错而不是全量
    /// 载入 + base64 内联。
    #[tokio::test]
    async fn read_tool_rejects_oversized_image() {
        use crate::in_memory::InMemoryExecutionEnv;
        use crate::ExecutionToolContext;

        let env = Arc::new(InMemoryExecutionEnv::with_cwd("/tmp/work".into()));
        let abs = env
            .absolute_path("big.jpg", None)
            .await
            .expect("absolute_path");
        // JPEG 魔数 + 填充到超过上限 1 字节。
        let mut bytes = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        bytes.resize(MAX_IMAGE_READ_BYTES + 1, 0);
        env.seed_file(&abs.to_string_lossy(), bytes).await;

        let env_dyn: Arc<dyn ExecutionEnv> = env.clone();
        let mut_env: Arc<dyn crate::file_mutation_queue::MutatingEnv> = env.clone();
        let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));
        let tool = create_read_tool(&ctx, None);

        let err = tool
            .execute(
                "read-img",
                serde_json::json!({ "path": "big.jpg" }),
                CancellationToken::new(),
                Arc::new(|_p| ()),
            )
            .await
            .expect_err("oversized image must be rejected");
        match err {
            AgentError::Tool(msg) => assert!(msg.contains("exceeds"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
