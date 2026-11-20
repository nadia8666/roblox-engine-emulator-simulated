//! EncodingService: CompressBuffer / DecompressBuffer / GetDecompressedBufferSize (Zstd, the one CompressionAlgorithm)
//! and Base64Encode / Base64Decode, on a client and a server alike, as Roblox's EncodingService documentation gives them:
//! compressionLevel 1 by default, -7 to 22. The zstd library is the one Roblox's documentation names. Measured in a 0.747
//! playing client and server (tools/parity/probes/encoding.luau): every member answers a buffer; an argument that is no
//! buffer is "missing or nil"; Base64Decode refuses input that is not padded Base64; the errors' texts are the client's.
//! ComputeBufferHash / ComputeStringHash are not emulated (Output `missing`).

use std::rc::Rc;

use mlua::prelude::*;

use super::{Behaviors, method, values};
use crate::{
    Engine,
    lua::{Args, Flow, raise, types::LEnumItem},
};

const MAX_DECOMPRESSED: u64 = 1 << 30;

/// (measured: a string, like nil, is "missing or nil")
fn buffer_argument(args: &Args, index: usize) -> Result<Vec<u8>, Flow> {
    match args.get(index) {
        LuaValue::Buffer(buffer) => Ok(buffer.to_vec()),
        _ => raise(format!("Argument {index} missing or nil")),
    }
}

fn algorithm_argument(engine: &Engine, args: &Args, index: usize, member: &str) -> Result<(), Flow> {
    let zstd = engine.reflection.enum_item("CompressionAlgorithm", "Zstd");
    match args.get(index) {
        LuaValue::UserData(ud) if ud.borrow::<LEnumItem>().is_ok_and(|item| Some(item.0) == zstd) => Ok(()),
        LuaValue::Nil => raise(format!("Argument {index} missing or nil")),
        _ => raise(format!("{member}: Enum.CompressionAlgorithm expected")),
    }
}

/// The decompressed size a Zstd frame holds, when it holds one within the limit.
fn decompressed_size(input: &[u8]) -> Option<u64> {
    zstd::zstd_safe::get_frame_content_size(input).ok().flatten().filter(|&size| size <= MAX_DECOMPRESSED)
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let Some(service) = reflection.class_id("EncodingService") else { return };
    let name = |member: &str| reflection.name_id(member);
    if let Some(n) = name("CompressBuffer") {
        behaviors.add_method(service, n, method(|lua, engine, _, args| {
            let input = buffer_argument(args, 1)?;
            algorithm_argument(engine, args, 2, "CompressBuffer")?;
            let level = match (args.get(3), args.number(3)) {
                (LuaValue::Nil, _) => 1.0,
                (_, Some(level)) => level,
                _ => return raise("CompressBuffer: compressionLevel must be a number"),
            };
            // (an int argument: a fraction is cut off; measured: 23 is refused)
            let level = level.trunc();
            if !(-7.0..=22.0).contains(&level) {
                return raise("CompressBuffer: unsupported compression level for Zstd");
            }
            let compressed = zstd::bulk::compress(&input, level as i32).or_else(|error| raise(format!("CompressBuffer: {error}")))?;
            Ok(values([LuaValue::Buffer(lua.create_buffer(compressed)?)]))
        }));
    }
    if let Some(n) = name("DecompressBuffer") {
        behaviors.add_method(service, n, method(|lua, engine, _, args| {
            let input = buffer_argument(args, 1)?;
            algorithm_argument(engine, args, 2, "DecompressBuffer")?;
            // (measured: data that is no Zstd frame; a frame without its size, or one that does not hold what it says, is
            // refused the same way here)
            const FAILED: &str = "DecompressBuffer: decompression failed - Unspecified error code";
            let Some(size) = decompressed_size(&input) else { return raise(FAILED) };
            let output = zstd::bulk::decompress(&input, size as usize).or_else(|_| raise(FAILED))?;
            if output.len() as u64 != size {
                return raise(FAILED);
            }
            Ok(values([LuaValue::Buffer(lua.create_buffer(output)?)]))
        }));
    }
    if let Some(n) = name("GetDecompressedBufferSize") {
        behaviors.add_method(service, n, method(|_, engine, _, args| {
            let input = buffer_argument(args, 1)?;
            algorithm_argument(engine, args, 2, "GetDecompressedBufferSize")?;
            Ok(values([decompressed_size(&input).map_or(LuaValue::Nil, |size| LuaValue::Number(size as f64))]))
        }));
    }
    if let Some(n) = name("Base64Encode") {
        behaviors.add_method(service, n, method(|lua, _, _, args| {
            let input = buffer_argument(args, 1)?;
            Ok(values([LuaValue::Buffer(lua.create_buffer(base64::encode(input))?)]))
        }));
    }
    if let Some(n) = name("Base64Decode") {
        behaviors.add_method(service, n, method(|lua, _, _, args| {
            let input = buffer_argument(args, 1)?;
            // (measured: unpadded input is refused)
            const INVALID: &str = "Base64Decode: input is not a base64 string";
            if input.len() % 4 != 0 {
                return raise(INVALID);
            }
            let decoded = base64::decode(input).or_else(|_| raise(INVALID))?;
            Ok(values([LuaValue::Buffer(lua.create_buffer(decoded)?)]))
        }));
    }
}
