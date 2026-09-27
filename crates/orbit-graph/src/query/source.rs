//! Bounded reads of live worktree source for graph queries.
//!
//! Sync indexes only regular files up to [`MAX_FILE_BYTES`]. A path can be
//! replaced or grown after that. Every query read therefore opens with
//! `O_NONBLOCK`, accepts only a regular file, and stops at an explicit byte
//! budget (`STD-03 §R22`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::GraphError;
use crate::sync::scanner::MAX_FILE_BYTES;

/// Extra bytes a [`read_show_window`] may pull so a budget cut can finish the
/// UTF-8 character it landed in. A code point is at most four bytes.
const UTF8_CHAR_TAIL: usize = 3;

/// Source slice returned by [`read_show_window`].
pub(super) struct ShowWindow {
    /// Bytes inside the caller's budget, on a char boundary when the span is
    /// valid UTF-8.
    pub bytes: Vec<u8>,
    /// Inclusive byte start of the indexed span.
    pub start: usize,
    /// Exclusive byte end of the indexed span.
    pub end: usize,
    /// Whether `bytes` is shorter than the indexed span.
    pub truncated: bool,
}

/// Read the indexed span of a regular file, but never more than `max_bytes`
/// plus one trailing UTF-8 character.
///
/// `operation` names the filesystem read. `span_operation` names a span that
/// does not fit the live file. The file length comes from `fstat`; the body
/// past the returned window is not read.
pub(super) fn read_show_window(
    path: &Path,
    operation: &'static str,
    span_operation: &'static str,
    span_start: i64,
    span_end: i64,
    max_bytes: usize,
    file_label: &str,
) -> Result<ShowWindow, GraphError> {
    let mut source = RegularSource::open(path, operation)?;
    let start = i64_to_usize(span_operation, span_start)?;
    let end = i64_to_usize(span_operation, span_end)?;
    let end_len = u64::try_from(end)
        .map_err(|source| GraphError::invalid_data(span_operation, source.to_string()))?;
    if start > end || end_len > source.len {
        return Err(GraphError::invalid_data(
            span_operation,
            format!(
                "invalid span {start}..{end} for {file_label} with {} bytes",
                source.len
            ),
        ));
    }
    let span_len = end - start;
    let read_len = show_read_len(span_len, max_bytes);
    let start_len = u64::try_from(start)
        .map_err(|source| GraphError::invalid_data(span_operation, source.to_string()))?;
    let buf = if read_len == 0 {
        Vec::new()
    } else {
        source.read_at(start_len, read_len, operation, path)?
    };
    let entire_span = read_len == span_len;
    let byte_count = bounded_source_len(buf.as_slice(), max_bytes, entire_span);
    let truncated = byte_count < span_len;
    let mut bytes = buf;
    bytes.truncate(byte_count);
    Ok(ShowWindow {
        bytes,
        start,
        end,
        truncated,
    })
}

/// Read a regular source file that still fits the sync cap.
///
/// A non-regular file is refused before any blocking read. A regular file
/// larger than [`MAX_FILE_BYTES`] is refused without reading its body: sync
/// would not have indexed it, so the live file is stale.
pub(super) fn read_indexed_source(
    path: &Path,
    operation: &'static str,
) -> Result<Vec<u8>, GraphError> {
    let mut source = RegularSource::open(path, operation)?;
    source.read_capped(operation, path)
}

struct RegularSource {
    file: File,
    len: u64,
}

impl RegularSource {
    fn open(path: &Path, operation: &'static str) -> Result<Self, GraphError> {
        let mut options = File::options();
        options.read(true);
        // `open` of a FIFO blocks until a writer arrives. `O_NONBLOCK` makes
        // that return immediately so the type check below can refuse it.
        // Linux ignores the flag on regular files; it is cleared anyway so a
        // later `read` cannot observe `EAGAIN`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = options
            .open(path)
            .map_err(|source| GraphError::io(operation, path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| GraphError::io(operation, path, source))?;
        if let Some(kind) = special_kind(&metadata) {
            return Err(GraphError::invalid_data(
                operation,
                format!(
                    "{} is a {kind}, not a regular file; query source reads only regular files",
                    path.display()
                ),
            ));
        }
        #[cfg(unix)]
        clear_nonblock(&file).map_err(|source| GraphError::io(operation, path, source))?;
        Ok(Self {
            file,
            len: metadata.len(),
        })
    }

    fn read_at(
        &mut self,
        start: u64,
        len: usize,
        operation: &'static str,
        path: &Path,
    ) -> Result<Vec<u8>, GraphError> {
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|source| GraphError::io(operation, path, source))?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(len).map_err(|source| {
            GraphError::invalid_data(
                operation,
                format!("refusing to allocate {len} source bytes: {source}"),
            )
        })?;
        buf.resize(len, 0);
        self.file
            .read_exact(buf.as_mut_slice())
            .map_err(|source| GraphError::io(operation, path, source))?;
        account(len);
        Ok(buf)
    }

    fn read_capped(&mut self, operation: &'static str, path: &Path) -> Result<Vec<u8>, GraphError> {
        if self.len > MAX_FILE_BYTES {
            return Err(oversized(operation, path, self.len));
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|source| GraphError::io(operation, path, source))?;
        let mut buf = Vec::new();
        self.file
            .by_ref()
            .take(MAX_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|source| GraphError::io(operation, path, source))?;
        account(buf.len());
        let read_len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if read_len > MAX_FILE_BYTES {
            return Err(oversized(operation, path, read_len));
        }
        Ok(buf)
    }
}

fn show_read_len(span_len: usize, max_bytes: usize) -> usize {
    if max_bytes == 0 || span_len == 0 {
        0
    } else if span_len <= max_bytes {
        span_len
    } else {
        span_len.min(max_bytes.saturating_add(UTF8_CHAR_TAIL))
    }
}

/// Prefix of `source` no longer than `max_bytes`.
///
/// When `entire_span` is set, `source` is the whole indexed span and the
/// historical rule applies: rewind to a char boundary only if that span is
/// valid UTF-8. Otherwise `source` is the budget plus at most three extra
/// bytes, and a split character is rewound when that character itself is
/// valid. Bytes past this window are not scanned; reading them would exceed
/// the caller's budget.
fn bounded_source_len(source: &[u8], max_bytes: usize, entire_span: bool) -> usize {
    let byte_count = source.len().min(max_bytes);
    if entire_span {
        if str::from_utf8(source).is_err() {
            return byte_count;
        }
        return rewind_to_char_boundary(source, byte_count);
    }
    match str::from_utf8(&source[..byte_count]) {
        Ok(_) => byte_count,
        Err(error) if error.error_len().is_none() => {
            let valid = error.valid_up_to();
            if valid < byte_count && utf8_has_char(&source[valid..]) {
                valid
            } else {
                byte_count
            }
        }
        Err(_) => byte_count,
    }
}

fn rewind_to_char_boundary(source: &[u8], mut byte_count: usize) -> usize {
    let Ok(text) = str::from_utf8(source) else {
        return byte_count;
    };
    while byte_count > 0 && !text.is_char_boundary(byte_count) {
        byte_count -= 1;
    }
    byte_count
}

fn utf8_has_char(bytes: &[u8]) -> bool {
    match str::from_utf8(bytes) {
        Ok(text) => !text.is_empty(),
        Err(error) => error.valid_up_to() > 0,
    }
}

fn oversized(operation: &'static str, path: &Path, len: u64) -> GraphError {
    GraphError::invalid_data(
        operation,
        format!(
            "{} is {len} bytes, over the {MAX_FILE_BYTES}-byte live source cap; the file grew after it was indexed",
            path.display()
        ),
    )
}

fn special_kind(metadata: &std::fs::Metadata) -> Option<&'static str> {
    if metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let file_type = metadata.file_type();
        if file_type.is_fifo() {
            return Some("fifo");
        }
        if file_type.is_socket() {
            return Some("socket");
        }
        if file_type.is_block_device() {
            return Some("block device");
        }
        if file_type.is_char_device() {
            return Some("character device");
        }
    }
    if metadata.is_dir() {
        return Some("directory");
    }
    Some("special file")
}

#[cfg(unix)]
fn clear_nonblock(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // `F_GETFL` / `F_SETFL` only touch this process's descriptor flags.
    // SAFETY: `fd` is the open file's descriptor. `F_GETFL`/`F_SETFL` only
    // read and replace this process's status flags; they do not close it.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let cleared = flags & !libc::O_NONBLOCK;
    if unsafe { libc::fcntl(fd, libc::F_SETFL, cleared) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn i64_to_usize(operation: &'static str, value: i64) -> Result<usize, GraphError> {
    usize::try_from(value).map_err(|source| GraphError::invalid_data(operation, source.to_string()))
}

#[cfg(test)]
thread_local! {
    static BYTES_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn account(bytes: usize) {
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    BYTES_READ.with(|cell| cell.set(cell.get().saturating_add(bytes)));
}

#[cfg(not(test))]
fn account(_bytes: usize) {}

#[cfg(test)]
pub(super) fn reset_bytes_read() {
    BYTES_READ.with(|cell| cell.set(0));
}

#[cfg(test)]
pub(super) fn bytes_read() -> u64 {
    BYTES_READ.with(|cell| cell.get())
}

#[cfg(test)]
#[path = "tests/source.rs"]
mod tests;
