//! The counter service's wire format: a profile (`--profile hw`, [`super`]) asks the service (tools/hw-counters) to count
//! the CPU's hardware events on its own process's threads, and gets back each thread's slices: every stretch it ran on a
//! CPU, from being switched in to being switched out, with what the counters counted meanwhile. Shared by both sides
//! (tools/hw-counters includes this file), so it uses nothing but std.
//!
//! A message is a little-endian u32 length (of what follows), a kind byte and its fields: integers little-endian, a
//! string a u16 length and UTF-8, a list a u32 count and its items.

/// The service's pipe, and the one `ree-hw-counters serve` (the service run in a console, for tests) listens on
pub const PIPE_NAME: &str = r"\\.\pipe\ree-hw-counters";
pub const CONSOLE_PIPE_NAME: &str = r"\\.\pipe\ree-hw-counters-console";
/// The Windows service's name (tools/hw-counters/setup.ps1 installs it)
pub const SERVICE_NAME: &str = "ree-hw-counters";
/// The protocol's version: a service that speaks another one says so instead of counting
pub const VERSION: u32 = 1;
/// The most counters one request names (the CPU's own limit is lower: the service checks it)
pub const MAX_COUNTERS: usize = 8;
/// The largest request the service reads, and the largest reply a client reads
pub const MAX_REQUEST_BYTES: u32 = 64 * 1024;
pub const MAX_REPLY_BYTES: u32 = 1 << 30;

// (the counters by name: counters.rs, shared with the profile viewer)
#[allow(unused_imports)]
pub use super::counters::{Custom, DEFAULT_COUNTERS, KNOWN, Known, known, name_of, source_of};

/// A client's request
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Count `counters` (Windows profile sources) on every thread of the asking process; `threads` are the ones it reads
    /// apart (its run thread)
    Start { version: u32, counters: Vec<String>, threads: Vec<u32> },
    Stop,
}

/// What a thread did on one CPU between being switched in and out, in the QueryPerformanceCounter clock, and what each
/// counter counted meanwhile (the order of the request's counters)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slice {
    pub cpu: u16,
    pub start: i64,
    pub end: i64,
    pub counts: [u64; MAX_COUNTERS],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thread {
    pub id: u32,
    /// One the request named
    pub registered: bool,
    pub slices: Vec<Slice>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Results {
    pub counters: u8,
    pub threads: Vec<Thread>,
    /// Context switches seen, and those of the process's threads that came without counter values once their CPU was
    /// counting (each costs a stretch: the switches before the counters were set are not among them)
    pub switches: u64,
    pub without_counters: u64,
    /// What the trace session lost (its buffers were full)
    pub lost_events: u32,
    pub lost_buffers: u32,
    pub notes: Vec<String>,
}

/// The service's replies
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Counting: the profile sources it counts (the request's order) and the clock's frequency
    Started { version: u32, sources: Vec<String>, qpc_frequency: u64, notes: Vec<String> },
    Error(String),
    Results(Results),
}

const REQUEST_START: u8 = 1;
const REQUEST_STOP: u8 = 2;
const REPLY_STARTED: u8 = 1;
const REPLY_ERROR: u8 = 2;
const REPLY_RESULTS: u8 = 3;

/// Message bytes being written: the length is filled in by [`Writer::finish`]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn new(kind: u8) -> Writer {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(&[0, 0, 0, 0, kind]);
        Writer { bytes }
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// (a string longer than a u16 counts is cut at a character boundary)
    pub fn string(&mut self, text: &str) {
        let mut end = text.len().min(u16::MAX as usize);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.bytes.extend_from_slice(&text.as_bytes()[..end]);
    }

    pub fn strings(&mut self, texts: &[String]) {
        self.u32(texts.len() as u32);
        for text in texts {
            self.string(text);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        let length = (self.bytes.len() - 4) as u32;
        self.bytes[..4].copy_from_slice(&length.to_le_bytes());
        self.bytes
    }
}

/// A message's fields being read: every read is bounds-checked
pub struct Fields<'a> {
    bytes: &'a [u8],
}

impl<'a> Fields<'a> {
    pub fn new(bytes: &'a [u8]) -> Fields<'a> {
        Fields { bytes }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if self.bytes.len() < count {
            return Err("the message ends early".into());
        }
        let (head, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn string(&mut self) -> Result<String, String> {
        let length = self.u16()? as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| "a string is not UTF-8".to_string())
    }

    /// A count of items each at least `item_bytes` long (a count the message cannot hold is refused before anything is
    /// allocated for it)
    pub fn count(&mut self, item_bytes: usize) -> Result<usize, String> {
        let count = self.u32()? as usize;
        if count.saturating_mul(item_bytes.max(1)) > self.bytes.len() {
            return Err("a list is longer than its message".into());
        }
        Ok(count)
    }

    pub fn strings(&mut self) -> Result<Vec<String>, String> {
        let count = self.count(2)?;
        (0..count).map(|_| self.string()).collect()
    }

    pub fn end(&self) -> Result<(), String> {
        if self.bytes.is_empty() { Ok(()) } else { Err("the message has bytes after its fields".into()) }
    }
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Request::Start { version, counters, threads } => {
                let mut writer = Writer::new(REQUEST_START);
                writer.u32(*version);
                writer.strings(counters);
                writer.u32(threads.len() as u32);
                for thread in threads {
                    writer.u32(*thread);
                }
                writer.finish()
            }
            Request::Stop => Writer::new(REQUEST_STOP).finish(),
        }
    }

    /// A request from its kind and fields (a message's bytes after the length)
    pub fn decode(kind: u8, body: &[u8]) -> Result<Request, String> {
        let mut fields = Fields::new(body);
        let request = match kind {
            REQUEST_START => {
                let version = fields.u32()?;
                let counters = fields.strings()?;
                let count = fields.count(4)?;
                let threads = (0..count).map(|_| fields.u32()).collect::<Result<Vec<_>, _>>()?;
                Request::Start { version, counters, threads }
            }
            REQUEST_STOP => Request::Stop,
            other => return Err(format!("no request of kind {other}")),
        };
        fields.end()?;
        Ok(request)
    }
}

impl Reply {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Reply::Started { version, sources, qpc_frequency, notes } => {
                let mut writer = Writer::new(REPLY_STARTED);
                writer.u32(*version);
                writer.strings(sources);
                writer.u64(*qpc_frequency);
                writer.strings(notes);
                writer.finish()
            }
            Reply::Error(message) => {
                let mut writer = Writer::new(REPLY_ERROR);
                writer.string(message);
                writer.finish()
            }
            Reply::Results(results) => {
                let mut writer = Writer::new(REPLY_RESULTS);
                let counters = (results.counters as usize).min(MAX_COUNTERS);
                writer.u8(counters as u8);
                writer.u32(results.threads.len() as u32);
                for thread in &results.threads {
                    writer.u32(thread.id);
                    writer.u8(thread.registered as u8);
                    writer.u32(thread.slices.len() as u32);
                    for slice in &thread.slices {
                        writer.u16(slice.cpu);
                        writer.i64(slice.start);
                        writer.i64(slice.end);
                        for count in &slice.counts[..counters] {
                            writer.u64(*count);
                        }
                    }
                }
                writer.u64(results.switches);
                writer.u64(results.without_counters);
                writer.u32(results.lost_events);
                writer.u32(results.lost_buffers);
                writer.strings(&results.notes);
                writer.finish()
            }
        }
    }

    pub fn decode(kind: u8, body: &[u8]) -> Result<Reply, String> {
        let mut fields = Fields::new(body);
        let reply = match kind {
            REPLY_STARTED => Reply::Started { version: fields.u32()?, sources: fields.strings()?, qpc_frequency: fields.u64()?, notes: fields.strings()? },
            REPLY_ERROR => Reply::Error(fields.string()?),
            REPLY_RESULTS => {
                let counters = fields.u8()? as usize;
                if counters > MAX_COUNTERS {
                    return Err(format!("{counters} counters: at most {MAX_COUNTERS}"));
                }
                let thread_count = fields.count(9)?;
                let mut threads = Vec::with_capacity(thread_count);
                for _ in 0..thread_count {
                    let id = fields.u32()?;
                    let registered = fields.u8()? != 0;
                    let slice_count = fields.count(18 + 8 * counters)?;
                    let mut slices = Vec::with_capacity(slice_count);
                    for _ in 0..slice_count {
                        let mut slice = Slice { cpu: fields.u16()?, start: fields.i64()?, end: fields.i64()?, ..Slice::default() };
                        for count in &mut slice.counts[..counters] {
                            *count = fields.u64()?;
                        }
                        slices.push(slice);
                    }
                    threads.push(Thread { id, registered, slices });
                }
                Reply::Results(Results {
                    counters: counters as u8,
                    threads,
                    switches: fields.u64()?,
                    without_counters: fields.u64()?,
                    lost_events: fields.u32()?,
                    lost_buffers: fields.u32()?,
                    notes: fields.strings()?,
                })
            }
            other => return Err(format!("no reply of kind {other}")),
        };
        fields.end()?;
        Ok(reply)
    }
}

/// Reads one message: (kind, fields), refusing one longer than `max` bytes
pub fn read_message(reader: &mut impl std::io::Read, max: u32) -> Result<(u8, Vec<u8>), String> {
    let mut head = [0u8; 5];
    reader.read_exact(&mut head).map_err(|error| format!("reading a message: {error}"))?;
    let length = u32::from_le_bytes(head[..4].try_into().unwrap());
    if length == 0 || length > max {
        return Err(format!("a message of {length} bytes (at most {max})"));
    }
    let mut body = vec![0u8; length as usize - 1];
    reader.read_exact(&mut body).map_err(|error| format!("reading a message: {error}"))?;
    Ok((head[4], body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_reply(reply: &Reply) -> Reply {
        let bytes = reply.encode();
        let (kind, body) = read_message(&mut &bytes[..], MAX_REPLY_BYTES).unwrap();
        Reply::decode(kind, &body).unwrap()
    }

    /// Every message reads back as it was written, slices' counts included (as many as the request's counters)
    #[test]
    fn messages_round_trip() {
        let start = Request::Start { version: VERSION, counters: vec!["InstructionRetired".into(), "ReeL2Misses".into()], threads: vec![42, 7] };
        for request in [start, Request::Stop] {
            let bytes = request.encode();
            let (kind, body) = read_message(&mut &bytes[..], MAX_REQUEST_BYTES).unwrap();
            assert_eq!(Request::decode(kind, &body).unwrap(), request);
        }
        let started = Reply::Started { version: VERSION, sources: vec!["TotalCycles".into()], qpc_frequency: 10_000_000, notes: vec!["registered".into()] };
        assert_eq!(round_trip_reply(&started), started);
        assert_eq!(round_trip_reply(&Reply::Error("no".into())), Reply::Error("no".into()));
        let mut counts = [0u64; MAX_COUNTERS];
        counts[0] = u64::MAX;
        counts[1] = 3;
        let results = Results {
            counters: 2,
            threads: vec![Thread { id: 9, registered: true, slices: vec![Slice { cpu: 31, start: -5, end: i64::MAX, counts }] }, Thread { id: 10, registered: false, slices: Vec::new() }],
            switches: 1 << 40,
            without_counters: 2,
            lost_events: 3,
            lost_buffers: 4,
            notes: vec!["a".into(), "é".into()],
        };
        assert_eq!(round_trip_reply(&Reply::Results(results.clone())), Reply::Results(results));
    }

    /// A message that lies about its lengths is refused before anything is allocated for it
    #[test]
    fn a_message_that_lies_is_refused() {
        // a list of 2^32-1 threads in a few bytes
        let mut writer = Writer::new(REQUEST_START);
        writer.u32(VERSION);
        writer.u32(0);
        writer.u32(u32::MAX);
        let bytes = writer.finish();
        let (kind, body) = read_message(&mut &bytes[..], MAX_REQUEST_BYTES).unwrap();
        assert!(Request::decode(kind, &body).unwrap_err().contains("longer than its message"));
        // a message longer than the reader takes, and one that ends early
        let mut big = [0u8; 8];
        big[..4].copy_from_slice(&(MAX_REQUEST_BYTES + 1).to_le_bytes());
        assert!(read_message(&mut &big[..], MAX_REQUEST_BYTES).is_err());
        assert!(Request::decode(REQUEST_START, &[1, 0]).is_err());
        // bytes after the fields, and a kind nobody sends
        assert!(Request::decode(REQUEST_STOP, &[0]).is_err());
        assert!(Reply::decode(99, &[]).is_err());
    }

    /// Names: the known ones read their source (old names the same source), any other is a source of its own
    #[test]
    fn counter_names_map_to_sources() {
        assert_eq!(source_of("instructions"), "InstructionRetired");
        assert_eq!(source_of("llc-misses"), "ReeLlcMisses");
        assert_eq!(source_of("demand-fills-beyond-l3"), "ReeLlcMisses");
        assert_eq!(source_of("load-stall-cycles"), "ReeLoadStallCycles");
        assert_eq!(source_of("IcacheMisses"), "IcacheMisses");
        assert_eq!(name_of("ReeL2Misses"), "demand-fills-beyond-l2");
        assert_eq!(name_of("DcacheMisses"), "DcacheMisses");
        assert!(DEFAULT_COUNTERS.iter().all(|name| known(name).is_some()));
    }
}
