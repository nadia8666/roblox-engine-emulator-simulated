//! Captured script output (print / warn / error / engine diagnostics), mirrored to stderr when
//! verbose. Port of `runtime/engine/output.luau`.

use std::{cell::RefCell, io::Write, path::Path};

use rustc_hash::FxHashMap;

thread_local! {
    static CAPTURED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Writes an engine diagnostic line (verbose script output, mount timings) to stderr, or to this
/// thread's capture while one is open ([`capture`]: `ree serve` returns a job's lines to its client).
pub fn diagnostic(line: &str) {
    let captured = CAPTURED.with(|captured| match captured.borrow_mut().as_mut() {
        Some(text) => {
            text.push_str(line);
            text.push('\n');
            true
        }
        None => false,
    });
    if !captured {
        eprintln!("{line}");
    }
}

/// Writes `text`'s lines as [`diagnostic`] writes each (lines captured earlier, replayed).
pub fn replay(text: &str) {
    for line in text.lines() {
        diagnostic(line);
    }
}

/// Captures this thread's diagnostics until [`take_captured`].
pub fn capture() {
    CAPTURED.with(|captured| *captured.borrow_mut() = Some(String::new()));
}

/// The diagnostics captured since [`capture`]; ends the capture.
pub fn take_captured() -> String {
    CAPTURED.with(|captured| captured.borrow_mut().take()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: &'static str,
    pub message: String,
    /// Virtual time (Scheduler.Now) when it was written
    pub time: f64,
    pub trace: Option<String>,
}

impl Entry {
    /// The line as `ree run` prints it in its `output` array.
    pub fn json(&self) -> serde_json::Value {
        let mut line = serde_json::json!({ "kind": self.kind, "message": self.message, "time": self.time });
        if let Some(trace) = &self.trace {
            line["trace"] = serde_json::json!(trace);
        }
        line
    }
}

#[derive(Default, Debug)]
pub struct Output {
    pub entries: Vec<Entry>,
    pub verbose: bool,
    /// `Class.Member` of unemulated methods scripts called, with call counts (first-call order)
    pub missing: Vec<(String, u64)>,
    missing_index: FxHashMap<String, usize>,
    /// Where lines go instead of `entries` while one is set ([`Output::spool`])
    spool: Option<Box<Spool>>,
    log: Option<OutputLog>,
    log_error: Option<String>,
    /// A script printed a line that is exactly [`DONE`]: `ree run` / `ree record` stop at the end of the frame
    pub done: bool,
}

/// The line a harness prints to end its run early (record/run.rs).
pub const DONE: &str = "@done";

impl Output {
    pub fn add(&mut self, kind: &'static str, message: String, trace: Option<String>, time: f64) {
        if kind == "print" && message == DONE {
            self.done = true;
        }
        if self.verbose {
            let prefix = if kind == "print" { String::new() } else { format!("[{kind}] ") };
            diagnostic(&format!("{prefix}{message}"));
            if let (Some(trace), "error") = (&trace, kind) {
                diagnostic(trace);
            }
        }
        let entry = Entry { kind, message, time, trace };
        let log_error = self.log.as_mut().and_then(|log| log.write(&entry).err());
        if let Some(error) = log_error {
            self.log_error = Some(error.to_string());
            self.log = None;
        }
        match &mut self.spool {
            Some(spool) => spool.write(&entry),
            None => self.entries.push(entry),
        }
    }

    pub fn set_log(&mut self, path: &Path) -> Result<(), String> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("--output-log {}: {error}", path.display()))?;
        let mut log = OutputLog { file };
        for entry in &self.entries {
            log.write(entry).map_err(|error| format!("--output-log {}: {error}", path.display()))?;
        }
        self.log = Some(log);
        self.log_error = None;
        Ok(())
    }

    pub fn log_error(&self) -> Option<&str> {
        self.log_error.as_deref()
    }

    /// From now on, lines (and those kept so far) are written to a temporary file as `ree run` prints
    /// them ([`Entry::json`]) instead of kept in `entries`, and [`Output::take_spool`] returns them.
    /// False (and lines kept as before) when no temporary file can be made.
    pub fn spool(&mut self) -> bool {
        self.spool_as(run_line, b",")
    }

    /// [`Output::spool`] with each line written by `format`, `separator` between two (`ree record`'s
    /// events.json holds them otherwise).
    pub fn spool_as(&mut self, format: LineFormat, separator: &'static [u8]) -> bool {
        if self.spool.is_none() {
            self.spool = Spool::new(format, separator).map(Box::new);
            if let Some(spool) = &mut self.spool {
                for entry in self.entries.drain(..) {
                    spool.write(&entry);
                }
            }
        }
        self.spool.is_some()
    }

    /// The lines written since [`Output::spool`]; lines are kept in `entries` again from now on.
    pub fn take_spool(&mut self) -> Option<Box<Spool>> {
        self.spool.take()
    }

    /// Output.Missing: an unemulated member was used.
    pub fn missing(&mut self, class: &str, member: &str) {
        let key = format!("{class}.{member}");
        if let Some(&index) = self.missing_index.get(&key) {
            self.missing[index].1 += 1;
            return;
        }
        if self.verbose {
            diagnostic(&format!("[stub] {key} is not emulated; returned nil"));
        }
        self.missing_index.insert(key.clone(), self.missing.len());
        self.missing.push((key, 1));
    }
}

#[derive(Debug)]
struct OutputLog {
    file: std::fs::File,
}

impl OutputLog {
    fn write(&mut self, entry: &Entry) -> std::io::Result<()> {
        let line = serde_json::to_vec(&entry.json()).map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
        self.file.write_all(&line)?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }
}

/// Output lines written to a temporary file as they come, each as [`Entry::json`] with commas between:
/// the inside of `ree run`'s `output` array. A run printing a write trace (~600k lines) kept ~0.25-0.6
/// GB of lines and built a JSON of them several times that size at its end; a file's pages are the
/// system's cache, not the process's memory. The file is deleted when it is closed (Windows), or as
/// soon as it is open (elsewhere).
pub struct Spool {
    file: std::io::BufWriter<std::fs::File>,
    format: LineFormat,
    separator: &'static [u8],
    /// A line's bytes, reused
    line: Vec<u8>,
    lines: usize,
    errors: usize,
    warnings: usize,
    failed: Option<String>,
}

/// How a spooled line is written ([`Output::spool_as`]).
pub type LineFormat = fn(&Entry, &mut Vec<u8>);

/// A line as `ree run` prints it in its `output` array.
fn run_line(entry: &Entry, out: &mut Vec<u8>) {
    let _ = serde_json::to_writer(out, &entry.json());
}

impl std::fmt::Debug for Spool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spool").field("lines", &self.lines).field("errors", &self.errors).finish()
    }
}

impl Spool {
    fn new(format: LineFormat, separator: &'static [u8]) -> Option<Spool> {
        let file = temporary_file().ok()?;
        Some(Spool { file: std::io::BufWriter::with_capacity(1 << 16, file), format, separator, line: Vec::new(), lines: 0, errors: 0, warnings: 0, failed: None })
    }

    fn write(&mut self, entry: &Entry) {
        use std::io::Write;
        if self.failed.is_none() {
            self.line.clear();
            (self.format)(entry, &mut self.line);
            let written = if self.lines > 0 { self.file.write_all(self.separator) } else { Ok(()) }.and_then(|()| self.file.write_all(&self.line));
            if let Err(error) = written {
                self.failed = Some(error.to_string());
            }
        }
        self.lines += 1;
        self.errors += (entry.kind == "error") as usize;
        self.warnings += (entry.kind == "warn") as usize;
    }

    /// The lines of kind `warn` written.
    pub fn warnings(&self) -> usize {
        self.warnings
    }

    /// The lines written.
    pub fn lines(&self) -> usize {
        self.lines
    }

    /// The lines of kind `error` written.
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// The file, to read the lines' JSON from its start.
    pub fn finish(self) -> Result<std::fs::File, String> {
        use std::io::Seek;
        if let Some(error) = self.failed {
            return Err(format!("output lines: {error}"));
        }
        let mut file = self.file.into_inner().map_err(|error| format!("output lines: {}", error.error()))?;
        file.seek(std::io::SeekFrom::Start(0)).map_err(|error| format!("output lines: {error}"))?;
        Ok(file)
    }
}

/// A new file in the temporary directory, read and written, that is gone once closed.
fn temporary_file() -> std::io::Result<std::fs::File> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let directory = std::env::temp_dir();
    loop {
        let count = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = directory.join(format!("ree-output-{}-{count}.json", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_ATTRIBUTE_TEMPORARY (kept in the cache while memory allows), FILE_FLAG_DELETE_ON_CLOSE
            options.attributes(0x100).custom_flags(0x0400_0000);
        }
        match options.open(&path) {
            Ok(file) => {
                #[cfg(not(windows))]
                let _ = std::fs::remove_file(&path);
                return Ok(file);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && count < 1 << 20 => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod output_log_tests {
    use super::{Entry, Output};

    #[test]
    fn output_log_writes_entries_as_they_arrive() {
        let path = std::env::temp_dir().join(format!("ree-output-log-{}.jsonl", std::process::id()));
        let mut output = Output::default();
        output.set_log(&path).unwrap();
        output.add("print", "first".into(), None, 1.0);
        output.add("warn", "second".into(), Some("trace".into()), 2.0);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(lines, vec![
            Entry { kind: "print", message: "first".into(), time: 1.0, trace: None }.json(),
            Entry { kind: "warn", message: "second".into(), time: 2.0, trace: Some("trace".into()) }.json(),
        ]);
        drop(output);
        let _ = std::fs::remove_file(path);
    }
}
