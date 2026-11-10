//! Host registration is process-wide, so this regression has its own test binary.

// The engine's service code links mi_collect, as the other engine integration tests do.
extern crate mimalloc;

use mlua::{Function, Lua, Table, ffi};
use roblox_engine_emulator::cost;

unsafe extern "C-unwind" {
    fn lua_sethostcfunction(function: ffi::lua_CFunction);
}

/// How many functions a block of the VM's registry holds (lapi.cpp kHostFunctionsPerBlock)
const BLOCK_SIZE: usize = 256;
/// The registered batch fills two blocks, so the trampoline mlua appends opens a third
const BATCH_SIZE: usize = 2 * BLOCK_SIZE;
/// The callback registered on its own, after the batch and the trampoline
const SINGLE_INDEX: usize = BATCH_SIZE;
/// The callback never registered: the control that shows the counters are live
const UNREGISTERED_INDEX: usize = BATCH_SIZE + 1;

/// A host callback that makes a table holding `INDEX`, the one allocation a library function would be priced for. Each index is a
/// function of its own, so the registry sees a distinct address per callback.
unsafe extern "C-unwind" fn host<const INDEX: usize>(state: *mut ffi::lua_State) -> i32 {
    unsafe {
        ffi::lua_createtable(state, 1, 0);
        ffi::lua_pushinteger(state, INDEX as ffi::lua_Integer);
        ffi::lua_rawseti(state, -2, 1);
    }
    1
}

/// Callbacks 0 through `SINGLE_INDEX` (the unregistered control is made apart), in rows of sixteen. A function pointer cannot be made from a number at run time, so each
/// index is written out.
fn callbacks() -> Vec<ffi::lua_CFunction> {
    macro_rules! list {
        ($($index:literal),* $(,)?) => { vec![$(host::<$index> as ffi::lua_CFunction),*] };
    }
    list![
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47,
        48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
        64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79,
        80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
        96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111,
        112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
        128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143,
        144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159,
        160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175,
        176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191,
        192, 193, 194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207,
        208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223,
        224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239,
        240, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255,
        256, 257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 267, 268, 269, 270, 271,
        272, 273, 274, 275, 276, 277, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287,
        288, 289, 290, 291, 292, 293, 294, 295, 296, 297, 298, 299, 300, 301, 302, 303,
        304, 305, 306, 307, 308, 309, 310, 311, 312, 313, 314, 315, 316, 317, 318, 319,
        320, 321, 322, 323, 324, 325, 326, 327, 328, 329, 330, 331, 332, 333, 334, 335,
        336, 337, 338, 339, 340, 341, 342, 343, 344, 345, 346, 347, 348, 349, 350, 351,
        352, 353, 354, 355, 356, 357, 358, 359, 360, 361, 362, 363, 364, 365, 366, 367,
        368, 369, 370, 371, 372, 373, 374, 375, 376, 377, 378, 379, 380, 381, 382, 383,
        384, 385, 386, 387, 388, 389, 390, 391, 392, 393, 394, 395, 396, 397, 398, 399,
        400, 401, 402, 403, 404, 405, 406, 407, 408, 409, 410, 411, 412, 413, 414, 415,
        416, 417, 418, 419, 420, 421, 422, 423, 424, 425, 426, 427, 428, 429, 430, 431,
        432, 433, 434, 435, 436, 437, 438, 439, 440, 441, 442, 443, 444, 445, 446, 447,
        448, 449, 450, 451, 452, 453, 454, 455, 456, 457, 458, 459, 460, 461, 462, 463,
        464, 465, 466, 467, 468, 469, 470, 471, 472, 473, 474, 475, 476, 477, 478, 479,
        480, 481, 482, 483, 484, 485, 486, 487, 488, 489, 490, 491, 492, 493, 494, 495,
        496, 497, 498, 499, 500, 501, 502, 503, 504, 505, 506, 507, 508, 509, 510, 511,
        512,
    ]
}

/// What a call added to the VM's allocation counters, which a library function is priced by and a host function is not
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct AllocationWork {
    library_objects: u64,
    allocated_bytes: u64,
    collector_objects: u64,
}

impl AllocationWork {
    fn now(lua: &Lua) -> Self {
        let counters = unsafe { &*cost::lua_workcounters(lua.current_thread().state()) };
        AllocationWork {
            library_objects: counters.hostops[cost::LIBOBJECTS_SLOT],
            allocated_bytes: counters.hostops[cost::ALLOCBYTES_SLOT],
            collector_objects: counters.hostops[cost::GCOBJECTS_SLOT],
        }
    }

    fn since(self, earlier: AllocationWork) -> AllocationWork {
        AllocationWork {
            library_objects: self.library_objects - earlier.library_objects,
            allocated_bytes: self.allocated_bytes - earlier.allocated_bytes,
            collector_objects: self.collector_objects - earlier.collector_objects,
        }
    }
}

struct Call {
    /// The number the callback put in its table
    value: i32,
    work: AllocationWork,
}

/// Calls `callback` from a Luau function, as a script does, and reads the table it made and what the call allocated
fn call(lua: &Lua, driver: &Function, callback: &Function) -> mlua::Result<Call> {
    let before = AllocationWork::now(lua);
    let value = driver.call::<i32>(callback)?;
    let work = AllocationWork::now(lua).since(before);
    Ok(Call { value, work })
}

fn host_closure(lua: &Lua, callback: ffi::lua_CFunction) -> mlua::Result<Function> {
    unsafe { lua.exec_raw::<Function>((), |state| ffi::lua_pushcfunction(state, callback)) }
}

#[test]
fn every_registered_callback_and_the_appended_trampoline_stay_host_work() -> mlua::Result<()> {
    let lua = Lua::new();
    lua.enable_jit(false);
    let callbacks = callbacks();
    let mut addresses: Vec<usize> = callbacks.iter().map(|callback| *callback as usize).collect();
    addresses.sort_unstable();
    addresses.dedup();
    assert_eq!(addresses.len(), SINGLE_INDEX + 1, "callbacks must have distinct machine addresses");

    // A batch that fills two blocks must still get mlua's trampoline appended; then the individual API adds one more callback.
    cost::mark_host_functions(&lua, &callbacks[..BATCH_SIZE])?;
    unsafe { lua_sethostcfunction(callbacks[SINGLE_INDEX]) };
    // Registering the first block's functions again adds nothing (every one is a duplicate).
    cost::mark_host_functions(&lua, &callbacks[..BLOCK_SIZE])?;

    let driver_source = r#"
        return function(callback)
            return callback()[1]
        end
    "#;
    let driver: Function = lua.load(driver_source).eval()?;

    let mut misclassified = Vec::new();
    for (index, callback) in callbacks.iter().enumerate() {
        let function = host_closure(&lua, *callback)?;
        // The first call classifies the closure; the second reads the answer it cached.
        for _ in 0..2 {
            let result = call(&lua, &driver, &function)?;
            assert_eq!(result.value, index as i32);
            if result.work != AllocationWork::default() {
                misclassified.push((index, result.work));
            }
        }
    }

    // A function made from a Rust closure runs through the trampoline.
    let trampoline_function = lua.create_function(|lua, ()| -> mlua::Result<Table> { lua.create_sequence_from([999]) })?;
    let trampoline = call(&lua, &driver, &trampoline_function)?;
    assert_eq!(trampoline.value, 999);
    println!(
        "host callbacks: {} checked twice, {} misclassified calls; first {:?}; trampoline {:?}",
        callbacks.len(),
        misclassified.len(),
        misclassified.first(),
        trampoline.work,
    );

    // The counters are live: an unregistered callback making the same table must pay the library allocation.
    let unregistered = host_closure(&lua, host::<UNREGISTERED_INDEX>)?;
    let control = call(&lua, &driver, &unregistered)?;
    assert_eq!(control.value, UNREGISTERED_INDEX as i32);
    assert_eq!(control.work.library_objects, 1);
    assert!(control.work.allocated_bytes > 0);
    assert_eq!(control.work.collector_objects, 1);

    assert!(
        misclassified.is_empty(),
        "{} registered calls were priced as library work; first {:?}",
        misclassified.len(),
        misclassified.first(),
    );
    assert_eq!(trampoline.work, AllocationWork::default(), "the appended trampoline stays host work too");
    Ok(())
}
