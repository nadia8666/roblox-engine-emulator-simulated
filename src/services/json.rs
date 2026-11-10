//! HttpService:JSONEncode and JSONDecode as a playing client runs them (measured in Studio 0.744:
//! tools/parity/probes/json.luau, json_numbers.luau, json_more.luau).
//!
//! Encoding:
//! - A table whose border (`#t`) is not 0 is an array of indices 1 to `#t` (a hole, or a value JSON cannot hold, is
//!   `null`); any other table is an object in the table's own iteration order, its keys named as BindableEvent:Fire
//!   names them (lua/bindable.rs: a number truncated to a 64-bit integer, `<Instance> (Name)`, `<Vector3> (1, 2, 3)`;
//!   a boolean or thread key raises `Invalid table key type used`). An empty table is `[]`. Metatables are not
//!   consulted; a table inside itself raises `tables cannot be cyclic`, nesting past [`ENCODE_DEPTH`] `Max lua stack
//!   size reached`.
//! - A number has Grisu2's digits (Loitsch 2010, the rapidjson form) with its last digit rounded toward the value only
//!   when at most eight digits came from the fraction loop (600 of 600 random doubles in json_numbers.luau), laid out
//!   as ECMAScript's Number::toString lays digits out but without `+` in an exponent (`1e21`, `100000000000000000000`,
//!   `1e-7`, `0.0000123`), with `-0` kept and infinities and NaN as `{"m":null,"t":"numeric","v":"inf"}` (`-inf`,
//!   `nan`).
//! - A string escapes `"` and `\`, writes `\b \f \n \r \t`, the other control bytes as `\u00XX` and everything else
//!   as it is (DEL, `/`, U+2028); one that is not UTF-8 raises `Can't convert to JSON`.
//! - A buffer is `{"m":null,"t":"buffer","base64":"..."}`; functions, threads, Instances, vectors, EnumItems and the
//!   other datatypes are `null`.
//!
//! Decoding is strict JSON (no trailing commas, leading zeros, `+`, `.5`, `5.`, NaN or Infinity; no raw control byte
//! in a string, no lone surrogate escape), with other string bytes kept as they are, even when they are not UTF-8.
//! Numbers are correctly rounded (an integer without fraction or exponent is never -0) and one that overflows is an
//! error. `null` leaves a hole in an array and no key in an object; the last of duplicate keys wins. A syntax error,
//! or nesting past [`DECODE_PARSE_DEPTH`], is `Can't parse JSON`; a document nested past [`DECODE_CONVERT_DEPTH`] but
//! parsed is `WebParser conversion stack overflow`.

use std::ffi::c_void;

use mlua::prelude::*;
use rustc_hash::FxHashMap;

use crate::Engine;

/// What JSONEncode and JSONDecode handled on this thread (os.workcounters' `jsonbytes` and `jsonvalues`; the work clock prices each
/// call by its own: cost.rs `json`)
#[derive(Clone, Copy, Default)]
pub struct JsonWork {
    /// Bytes of JSON text
    pub bytes: u64,
    /// Values of the documents: each table, string, number and boolean
    pub values: u64,
}

thread_local! {
    static HANDLED: std::cell::Cell<JsonWork> = const { std::cell::Cell::new(JsonWork { bytes: 0, values: 0 }) };
}

/// What JSONEncode and JSONDecode handled on this thread so far
pub fn counts() -> JsonWork {
    HANDLED.with(std::cell::Cell::get)
}

fn count(bytes: usize, values: u64) {
    HANDLED.with(|handled| {
        let before = handled.get();
        handled.set(JsonWork { bytes: before.bytes + bytes as u64, values: before.values + values });
    });
}

/// What a JSONEncode or JSONDecode of `bytes` of text holding `values` values costs the work clock (cost.rs [`crate::cost::json`]),
/// and counts them
pub(crate) fn priced(bytes: usize, values: u64, decode: bool) -> f64 {
    count(bytes, values);
    crate::cost::json(bytes, values, decode)
}

/// Tables JSONEncode nests: 300 encode and 301 raise, arrays and dictionaries alike, however deep the caller's own
/// stack is (json_limits.luau).
pub(crate) const ENCODE_DEPTH: usize = 300;
/// Arrays and objects JSONDecode parses nested: 1024 parse and 1025 do not (json_limits.luau).
pub(crate) const DECODE_PARSE_DEPTH: usize = 1024;
/// Values JSONDecode turns into Luau nested, a scalar counted as a level of its own: 401 empty arrays inside one another
/// convert and 402 do not, and 400 objects around a number convert and 401 do not (json_limits.luau).
pub(crate) const DECODE_CONVERT_DEPTH: usize = 401;

const NAN: &str = r#"{"m":null,"t":"numeric","v":"nan"}"#;
const INF: &str = r#"{"m":null,"t":"numeric","v":"inf"}"#;
const NEG_INF: &str = r#"{"m":null,"t":"numeric","v":"-inf"}"#;

/// A result and the number of JSON values that were written or made for it, which price the call ([`priced`])
pub(crate) struct Counted<T> {
    pub result: T,
    pub values: u64,
}

/// HttpService:JSONEncode of `value`, or the message it raises. The writers add each value they write to `values`, one count for the
/// whole call.
pub(crate) fn encode_counted(lua: &Lua, engine: &Engine, value: &LuaValue) -> Result<Counted<String>, String> {
    let mut text = String::new();
    let mut values = 0;
    write_value(lua, engine, value, &mut text, &mut Vec::new(), &mut values)?;
    Ok(Counted { result: text, values })
}

fn write_value(lua: &Lua, engine: &Engine, value: &LuaValue, out: &mut String, path: &mut Vec<*const c_void>, values: &mut u64) -> Result<(), String> {
    *values += 1;
    match value {
        LuaValue::Nil => out.push_str("null"),
        LuaValue::Boolean(flag) => out.push_str(if *flag { "true" } else { "false" }),
        LuaValue::Integer(number) => write_number(*number as f64, out),
        LuaValue::Number(number) => write_number(*number, out),
        LuaValue::String(text) => write_string(&text.as_bytes(), out)?,
        LuaValue::Buffer(buffer) => {
            out.push_str(r#"{"m":null,"t":"buffer","base64":""#);
            out.push_str(&base64::encode(buffer.to_vec()));
            out.push_str(r#""}"#);
        }
        LuaValue::Table(table) => {
            let pointer = table.to_pointer();
            if path.contains(&pointer) {
                return Err("tables cannot be cyclic".into());
            }
            if path.len() >= ENCODE_DEPTH {
                return Err("Max lua stack size reached".into());
            }
            path.push(pointer);
            let written = write_table(lua, engine, table, out, path, values);
            path.pop();
            written?;
        }
        _ => out.push_str("null"),
    }
    Ok(())
}

fn write_table(lua: &Lua, engine: &Engine, table: &LuaTable, out: &mut String, path: &mut Vec<*const c_void>, values: &mut u64) -> Result<(), String> {
    let length = table.raw_len();
    if length > 0 {
        out.push('[');
        for index in 1..=length {
            if index > 1 {
                out.push(',');
            }
            let item: LuaValue = table.raw_get(index).map_err(|error| error.to_string())?;
            write_value(lua, engine, &item, out, path, values)?;
        }
        out.push(']');
        return Ok(());
    }
    // An object: each key as a bindable names it; keys that end up equal keep the later value (as a bindable's copy
    // does), in the first one's place
    let mut entries: Vec<(Vec<u8>, String)> = Vec::new();
    let mut places: FxHashMap<Vec<u8>, usize> = FxHashMap::default();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, item) = pair.map_err(|error| error.to_string())?;
        let key = crate::lua::bindable::key_name(lua, engine, &key)?;
        let mut text = String::new();
        write_value(lua, engine, &item, &mut text, path, values)?;
        match places.get(&key) {
            Some(&place) => entries[place].1 = text,
            None => {
                places.insert(key.clone(), entries.len());
                entries.push((key, text));
            }
        }
    }
    if entries.is_empty() {
        out.push_str("[]");
        return Ok(());
    }
    out.push('{');
    for (index, (key, text)) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(key, out)?;
        out.push(':');
        out.push_str(text);
    }
    out.push('}');
    Ok(())
}

fn write_string(bytes: &[u8], out: &mut String) -> Result<(), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Can't convert to JSON".to_string())?;
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", control as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    Ok(())
}

/// A number's text (the module documentation's rules).
pub(crate) fn write_number(value: f64, out: &mut String) {
    if value.is_nan() {
        return out.push_str(NAN);
    }
    if value.is_infinite() {
        return out.push_str(if value > 0.0 { INF } else { NEG_INF });
    }
    if value == 0.0 {
        return out.push_str(if value.is_sign_negative() { "-0" } else { "0" });
    }
    if value < 0.0 {
        out.push('-');
    }
    let mut digits = [0u8; 20];
    let (length, exponent) = grisu2(value.abs(), &mut digits);
    let digits = std::str::from_utf8(&digits[..length]).expect("ASCII digits");
    // ECMAScript's Number::toString: n is where the decimal point falls in the digits
    let (k, n) = (length as i32, length as i32 + exponent);
    if k <= n && n <= 21 {
        out.push_str(digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push_str(&(n - 1).to_string());
    }
}

// Grisu2: a DiyFp is a 64-bit significand and a binary exponent

#[derive(Clone, Copy)]
struct DiyFp {
    f: u64,
    e: i32,
}

impl DiyFp {
    fn normalize(self) -> Self {
        let shift = self.f.leading_zeros();
        DiyFp { f: self.f << shift, e: self.e - shift as i32 }
    }

    /// The product's upper 64 bits, rounded at bit 63 of the lower half.
    fn multiply(self, other: Self) -> Self {
        let (a, b) = (self.f >> 32, self.f & 0xFFFF_FFFF);
        let (c, d) = (other.f >> 32, other.f & 0xFFFF_FFFF);
        let (ac, bc, ad, bd) = (a * c, b * c, a * d, b * d);
        let middle = (bd >> 32) + (ad & 0xFFFF_FFFF) + (bc & 0xFFFF_FFFF) + (1 << 31);
        DiyFp { f: ac + (ad >> 32) + (bc >> 32) + (middle >> 32), e: self.e + other.e + 64 }
    }
}

/// `value`'s digits into `buffer` (at most 17) and the decimal exponent of the last one: value ~ digits * 10^exponent.
fn grisu2(value: f64, buffer: &mut [u8; 20]) -> (usize, i32) {
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let mantissa = bits & ((1 << 52) - 1);
    let (f, e) = if biased != 0 { (mantissa | (1 << 52), biased - 1075) } else { (mantissa, -1074) };
    let plus = DiyFp { f: (f << 1) + 1, e: e - 1 }.normalize();
    let mut minus = if f == 1 << 52 && biased > 1 { DiyFp { f: (f << 2) - 1, e: e - 2 } } else { DiyFp { f: (f << 1) - 1, e: e - 1 } };
    minus.f <<= minus.e - plus.e;
    minus.e = plus.e;
    // the cached power that brings the boundaries' exponent into the digit loops' window
    let dk = f64::from(-61 - plus.e) * 0.301_029_995_663_981_14 + 347.0;
    let mut k = dk as i32;
    if dk - f64::from(k) > 0.0 {
        k += 1;
    }
    let index = ((k >> 3) + 1) as usize;
    let exponent = 348 - (index as i32) * 8;
    let (cf, ce) = CACHED_POWERS[index];
    let power = DiyFp { f: cf, e: i32::from(ce) };
    let w = DiyFp { f, e }.normalize().multiply(power);
    let mut upper = plus.multiply(power);
    let mut lower = minus.multiply(power);
    lower.f += 1;
    upper.f -= 1;
    digit_gen(w, upper, upper.f - lower.f, buffer, exponent)
}

const POW10: [u64; 20] = [
    1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000, 10_000_000_000, 100_000_000_000,
    1_000_000_000_000, 10_000_000_000_000, 100_000_000_000_000, 1_000_000_000_000_000, 10_000_000_000_000_000,
    100_000_000_000_000_000, 1_000_000_000_000_000_000, 10_000_000_000_000_000_000,
];

fn digit_gen(w: DiyFp, upper: DiyFp, mut delta: u64, buffer: &mut [u8; 20], mut exponent: i32) -> (usize, i32) {
    let shift = (-upper.e) as u32;
    let one = 1u64 << shift;
    let wp_w = upper.f.wrapping_sub(w.f);
    let mut p1 = (upper.f >> shift) as u32;
    let mut p2 = upper.f & (one - 1);
    let mut kappa = if p1 == 0 { 0 } else { p1.ilog10() as i32 + 1 };
    let mut length = 0;
    while kappa > 0 {
        let divisor = POW10[(kappa - 1) as usize] as u32;
        let digit = p1 / divisor;
        p1 %= divisor;
        if digit != 0 || length > 0 {
            buffer[length] = b'0' + digit as u8;
            length += 1;
        }
        kappa -= 1;
        let rest = (u64::from(p1) << shift) + p2;
        if rest <= delta {
            grisu_round(buffer, length, delta, rest, POW10[kappa as usize] << shift, wp_w);
            return (length, exponent + kappa);
        }
    }
    loop {
        p2 = p2.wrapping_mul(10);
        delta = delta.wrapping_mul(10);
        let digit = (p2 >> shift) as u8;
        if digit != 0 || length > 0 {
            buffer[length] = b'0' + digit;
            length += 1;
        }
        p2 &= one - 1;
        kappa -= 1;
        if p2 < delta {
            exponent += kappa;
            // The distance to the value is scaled to the fraction digits made (10^steps) for at most eight of them; past
            // that the client rounds nothing (measured: every case with nine or more kept Grisu2's upper digits)
            let steps = (-kappa) as usize;
            let scale = if steps <= 8 { POW10[steps] } else { 0 };
            grisu_round(buffer, length, delta, p2, one, wp_w.wrapping_mul(scale));
            return (length, exponent);
        }
    }
}

fn grisu_round(buffer: &mut [u8; 20], length: usize, delta: u64, mut rest: u64, ten_kappa: u64, wp_w: u64) {
    while rest < wp_w
        && delta.wrapping_sub(rest) >= ten_kappa
        && (rest.wrapping_add(ten_kappa) < wp_w || wp_w.wrapping_sub(rest) > rest.wrapping_add(ten_kappa).wrapping_sub(wp_w))
    {
        buffer[length - 1] -= 1;
        rest = rest.wrapping_add(ten_kappa);
    }
}

/// 10^k for k = -348, -340, ..., 340: the 64-bit significand rounded to nearest and the binary exponent.
const CACHED_POWERS: [(u64, i16); 87] = [
    (0xFA8FD5A0081C0288, -1220), (0xBAAEE17FA23EBF76, -1193), (0x8B16FB203055AC76, -1166),
    (0xCF42894A5DCE35EA, -1140), (0x9A6BB0AA55653B2D, -1113), (0xE61ACF033D1A45DF, -1087),
    (0xAB70FE17C79AC6CA, -1060), (0xFF77B1FCBEBCDC4F, -1034), (0xBE5691EF416BD60C, -1007), (0x8DD01FAD907FFC3C, -980),
    (0xD3515C2831559A83, -954), (0x9D71AC8FADA6C9B5, -927), (0xEA9C227723EE8BCB, -901), (0xAECC49914078536D, -874),
    (0x823C12795DB6CE57, -847), (0xC21094364DFB5637, -821), (0x9096EA6F3848984F, -794), (0xD77485CB25823AC7, -768),
    (0xA086CFCD97BF97F4, -741), (0xEF340A98172AACE5, -715), (0xB23867FB2A35B28E, -688), (0x84C8D4DFD2C63F3B, -661),
    (0xC5DD44271AD3CDBA, -635), (0x936B9FCEBB25C996, -608), (0xDBAC6C247D62A584, -582), (0xA3AB66580D5FDAF6, -555),
    (0xF3E2F893DEC3F126, -529), (0xB5B5ADA8AAFF80B8, -502), (0x87625F056C7C4A8B, -475), (0xC9BCFF6034C13053, -449),
    (0x964E858C91BA2655, -422), (0xDFF9772470297EBD, -396), (0xA6DFBD9FB8E5B88F, -369), (0xF8A95FCF88747D94, -343),
    (0xB94470938FA89BCF, -316), (0x8A08F0F8BF0F156B, -289), (0xCDB02555653131B6, -263), (0x993FE2C6D07B7FAC, -236),
    (0xE45C10C42A2B3B06, -210), (0xAA242499697392D3, -183), (0xFD87B5F28300CA0E, -157), (0xBCE5086492111AEB, -130),
    (0x8CBCCC096F5088CC, -103), (0xD1B71758E219652C, -77), (0x9C40000000000000, -50), (0xE8D4A51000000000, -24),
    (0xAD78EBC5AC620000, 3), (0x813F3978F8940984, 30), (0xC097CE7BC90715B3, 56), (0x8F7E32CE7BEA5C70, 83),
    (0xD5D238A4ABE98068, 109), (0x9F4F2726179A2245, 136), (0xED63A231D4C4FB27, 162), (0xB0DE65388CC8ADA8, 189),
    (0x83C7088E1AAB65DB, 216), (0xC45D1DF942711D9A, 242), (0x924D692CA61BE758, 269), (0xDA01EE641A708DEA, 295),
    (0xA26DA3999AEF774A, 322), (0xF209787BB47D6B85, 348), (0xB454E4A179DD1877, 375), (0x865B86925B9BC5C2, 402),
    (0xC83553C5C8965D3D, 428), (0x952AB45CFA97A0B3, 455), (0xDE469FBD99A05FE3, 481), (0xA59BC234DB398C25, 508),
    (0xF6C69A72A3989F5C, 534), (0xB7DCBF5354E9BECE, 561), (0x88FCF317F22241E2, 588), (0xCC20CE9BD35C78A5, 614),
    (0x98165AF37B2153DF, 641), (0xE2A0B5DC971F303A, 667), (0xA8D9D1535CE3B396, 694), (0xFB9B7CD9A4A7443C, 720),
    (0xBB764C4CA7A44410, 747), (0x8BAB8EEFB6409C1A, 774), (0xD01FEF10A657842C, 800), (0x9B10A4E5E9913129, 827),
    (0xE7109BFBA19C0C9D, 853), (0xAC2820D9623BF429, 880), (0x80444B5E7AA7CF85, 907), (0xBF21E44003ACDD2D, 933),
    (0x8E679C2F5E44FF8F, 960), (0xD433179D9C8CB841, 986), (0x9E19DB92B4E31BA9, 1013), (0xEB96BF6EBADF77D9, 1039),
    (0xAF87023B9BF0EE6B, 1066),
];

// Decoding

enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(Vec<u8>),
    Array(Vec<Json>),
    Object(Vec<(Vec<u8>, Json)>),
}

/// HttpService:JSONDecode of `text`, or the message it raises.
pub(crate) fn decode_counted(lua: &Lua, text: &[u8]) -> Result<Counted<LuaValue>, String> {
    let mut values = 0;
    let value = decode_inner(lua, text, &mut values)?;
    Ok(Counted { result: value, values })
}

fn decode_inner(lua: &Lua, text: &[u8], values: &mut u64) -> Result<LuaValue, String> {
    const SYNTAX: &str = "Can't parse JSON";
    let mut parser = Parser { bytes: text, index: 0, deepest: 0 };
    parser.skip_whitespace();
    let value = parser.value().ok_or(SYNTAX)?;
    parser.skip_whitespace();
    if parser.index != text.len() {
        return Err(SYNTAX.into());
    }
    if parser.deepest > DECODE_CONVERT_DEPTH {
        return Err("WebParser conversion stack overflow".into());
    }
    to_lua(lua, value, values).map_err(|error| error.to_string())
}

fn to_lua(lua: &Lua, value: Json, values: &mut u64) -> LuaResult<LuaValue> {
    *values += 1;
    Ok(match value {
        Json::Null => LuaValue::Nil,
        Json::Bool(flag) => LuaValue::Boolean(flag),
        Json::Number(number) => LuaValue::Number(number),
        Json::String(bytes) => LuaValue::String(lua.create_string(&bytes)?),
        Json::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (index, item) in items.into_iter().enumerate() {
                if !matches!(item, Json::Null) {
                    table.raw_set(index + 1, to_lua(lua, item, values)?)?;
                }
            }
            LuaValue::Table(table)
        }
        Json::Object(entries) => {
            let table = lua.create_table_with_capacity(0, entries.len())?;
            for (key, item) in entries {
                table.raw_set(lua.create_string(&key)?, to_lua(lua, item, values)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

struct Parser<'a> {
    bytes: &'a [u8],
    index: usize,
    /// The deepest value met, the top one at 1
    deepest: usize,
}

/// An array or object being parsed (the parser keeps them on a stack of its own: a document may nest thousands deep).
/// An object holds the key its next value goes to.
enum Open {
    Array(Vec<Json>),
    Object(Vec<(Vec<u8>, Json)>, Vec<u8>),
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.index += 1;
        }
    }

    fn literal(&mut self, word: &[u8], value: Json) -> Option<Json> {
        if self.bytes[self.index..].starts_with(word) {
            self.index += word.len();
            Some(value)
        } else {
            None
        }
    }

    /// One whole value, nested ones included.
    fn value(&mut self) -> Option<Json> {
        let mut stack: Vec<Open> = Vec::new();
        loop {
            // a value starts here: open a container, or read a scalar
            self.skip_whitespace();
            self.deepest = self.deepest.max(stack.len() + 1);
            let mut done = match self.peek()? {
                open @ (b'[' | b'{') => {
                    if stack.len() + 1 > DECODE_PARSE_DEPTH {
                        return None;
                    }
                    self.index += 1;
                    self.skip_whitespace();
                    let close = if open == b'[' { b']' } else { b'}' };
                    if self.peek() == Some(close) {
                        self.index += 1;
                        if open == b'[' { Json::Array(Vec::new()) } else { Json::Object(Vec::new()) }
                    } else if open == b'[' {
                        stack.push(Open::Array(Vec::new()));
                        continue;
                    } else {
                        let key = self.key()?;
                        stack.push(Open::Object(Vec::new(), key));
                        continue;
                    }
                }
                b'"' => Json::String(self.string()?),
                b't' => self.literal(b"true", Json::Bool(true))?,
                b'f' => self.literal(b"false", Json::Bool(false))?,
                b'n' => self.literal(b"null", Json::Null)?,
                b'-' | b'0'..=b'9' => self.number()?,
                _ => return None,
            };
            // hand the finished value to the containers it closes, until one wants another value
            loop {
                let Some(open) = stack.last_mut() else { return Some(done) };
                self.skip_whitespace();
                let more = match open {
                    Open::Array(items) => {
                        items.push(done);
                        match self.peek()? {
                            b',' => true,
                            b']' => false,
                            _ => return None,
                        }
                    }
                    Open::Object(entries, key) => {
                        let key = std::mem::take(key);
                        if matches!(done, Json::Null) {
                            // a null removes an earlier entry of the same key, as setting nil in a table does
                            entries.retain(|(existing, _)| existing != &key);
                        } else {
                            entries.push((key, done));
                        }
                        match self.peek()? {
                            b',' => true,
                            b'}' => false,
                            _ => return None,
                        }
                    }
                };
                self.index += 1;
                if more {
                    if let Some(Open::Object(_, key)) = stack.last_mut() {
                        self.skip_whitespace();
                        let next = self.key()?;
                        *key = next;
                    }
                    break;
                }
                done = match stack.pop()? {
                    Open::Array(items) => Json::Array(items),
                    Open::Object(entries, _) => Json::Object(entries),
                };
            }
        }
    }

    /// An object key and the colon after it.
    fn key(&mut self) -> Option<Vec<u8>> {
        if self.peek()? != b'"' {
            return None;
        }
        let key = self.string()?;
        self.skip_whitespace();
        if self.peek()? != b':' {
            return None;
        }
        self.index += 1;
        Some(key)
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = self.bytes.get(self.index..self.index + 4)?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        self.index += 4;
        u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    }

    fn string(&mut self) -> Option<Vec<u8>> {
        self.index += 1;
        let mut out = Vec::new();
        loop {
            let byte = self.peek()?;
            self.index += 1;
            match byte {
                b'"' => return Some(out),
                0..=0x1F => return None,
                b'\\' => {
                    let escape = self.peek()?;
                    self.index += 1;
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let code = match first {
                                0xD800..=0xDBFF => {
                                    if self.bytes.get(self.index..self.index + 2)? != b"\\u" {
                                        return None;
                                    }
                                    self.index += 2;
                                    let second = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&second) {
                                        return None;
                                    }
                                    0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return None,
                                code => code,
                            };
                            let mut encoded = [0u8; 4];
                            out.extend_from_slice(char::from_u32(code)?.encode_utf8(&mut encoded).as_bytes());
                        }
                        _ => return None,
                    }
                }
                other => out.push(other),
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.index;
        if self.peek() == Some(b'-') {
            self.index += 1;
        }
        match self.peek()? {
            b'0' => self.index += 1,
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        let mut integer = true;
        if self.peek() == Some(b'.') {
            integer = false;
            self.index += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return None;
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            integer = false;
            self.index += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.index += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return None;
            }
            self.digits();
        }
        let text = std::str::from_utf8(&self.bytes[start..self.index]).ok()?;
        let value: f64 = text.parse().ok()?;
        if value.is_infinite() {
            return None;
        }
        // an integer (no fraction, no exponent) reads as an integer would: "-0" is 0
        Some(Json::Number(if integer && value == 0.0 { 0.0 } else { value }))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each call counts its own document's values and leaves the thread's running totals (`counts`) to `priced`
    #[test]
    fn value_counts_are_local_to_a_document() {
        let lua = Lua::new();
        let engine = Engine::new(crate::Options::default()).unwrap();
        let totals_before = counts();

        // Six values: the outer array, 1, the object, the object's array b, true and false. The nulls are never made into Luau values.
        let decoded_text = br#"[1,null,{"a":null,"b":[true,false]}]"#;
        let decoded = decode_counted(&lua, decoded_text).unwrap();
        assert_eq!(decoded.values, 6);

        // The same six on the way out: the outer table, 1, the table holding b, b's array, true and false.
        let encoded_source = "return {1, {b = {true, false}}}";
        let encoded_value = lua.load(encoded_source).eval().unwrap();
        let encoded = encode_counted(&lua, &engine, &encoded_value).unwrap();
        assert_eq!(encoded.values, 6);

        // A scalar is one value, and a call that fails returns no count.
        assert_eq!(decode_counted(&lua, b"true").unwrap().values, 1);
        let cyclic = lua.create_table().unwrap();
        cyclic.raw_set(1, cyclic.clone()).unwrap();
        assert!(encode_counted(&lua, &engine, &LuaValue::Table(cyclic)).is_err());
        assert!(decode_counted(&lua, b"[1,").is_err());

        let totals_after = counts();
        assert_eq!(totals_after.bytes, totals_before.bytes);
        assert_eq!(totals_after.values, totals_before.values);
    }

    fn text(value: f64) -> String {
        let mut out = String::new();
        write_number(value, &mut out);
        out
    }

    /// The layout and the Grisu2 digits with the client's rounding (each case read from a capture).
    #[test]
    fn numbers_are_written_as_a_client_writes_them() {
        for (value, want) in [
            (0.1 + 0.2, "0.30000000000000007"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1e20, "100000000000000000000"),
            (1e21, "1e21"),
            (9.223372036854776e18, "9223372036854776000"),
            (1e300, "1e300"),
            (1e-7, "1e-7"),
            (5e-324, "5e-324"),
            (-0.0, "-0"),
            (123456789.125, "123456789.125"),
            (-5.0935662766632835e20, "-509356627666328350000"),
            (6.895221874975996e-182, "6.895221874975997e-182"),
            (1.8520430214829942e108, "1.8520430214829942e108"),
            (336.30000000000007, "336.30000000000009"),
            (0.0009171428571428571, "0.0009171428571428572"),
            (f64::INFINITY, r#"{"m":null,"t":"numeric","v":"inf"}"#),
        ] {
            assert_eq!(text(value), want, "{value:e}");
        }
    }
}
