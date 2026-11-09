// This file is part of the Luau programming language and is licensed under MIT License; see LICENSE.txt for details
// This code is based on Lua 5.x implementation licensed under MIT License; see lua_LICENSE.txt for details
#pragma once

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>

#include "luaconf.h"



// option for multiple returns in `lua_pcall' and `lua_call'
#define LUA_MULTRET (-1)

/*
** pseudo-indices
*/
#define LUA_REGISTRYINDEX (-LUAI_MAXCSTACK - 2000)
#define LUA_ENVIRONINDEX (-LUAI_MAXCSTACK - 2001)
#define LUA_GLOBALSINDEX (-LUAI_MAXCSTACK - 2002)
#define lua_upvalueindex(i) (LUA_GLOBALSINDEX - (i))
#define lua_ispseudo(i) ((i) <= LUA_REGISTRYINDEX)

// thread status; 0 is OK
enum lua_Status
{
    LUA_OK = 0,
    LUA_YIELD,
    LUA_ERRRUN,
    LUA_ERRSYNTAX, // legacy error code, preserved for compatibility
    LUA_ERRMEM,
    LUA_ERRERR,
    LUA_BREAK, // yielded for a debug breakpoint
};

enum lua_CoStatus
{
    LUA_CORUN = 0, // running
    LUA_COSUS,     // suspended
    LUA_CONOR,     // 'normal' (it resumed another coroutine)
    LUA_COFIN,     // finished
    LUA_COERR,     // finished with error
};

typedef struct lua_State lua_State;

typedef int (*lua_CFunction)(lua_State* L);
typedef int (*lua_Continuation)(lua_State* L, int status);

/*
** prototype for memory-allocation functions
*/

typedef void* (*lua_Alloc)(void* ud, void* ptr, size_t osize, size_t nsize);

// `type` is an opaque VM-defined value which identifies the caged heap allocation kind
// separating allocations of different types is recommended
typedef void* (*lua_CageAlloc)(void* ud, void* ptr, size_t osize, size_t nsize, int type);

// non-return type
#define l_noret void LUA_NORETURN

/*
** basic types
*/
#define LUA_TNONE (-1)

/*
 * WARNING: if you change the order of this enumeration,
 * grep "ORDER TYPE"
 */
// clang-format off
enum lua_Type
{
    LUA_TNIL = 0,     // must be 0 due to lua_isnoneornil
    LUA_TBOOLEAN = 1, // must be 1 due to l_isfalse

    LUA_TLIGHTUSERDATA,
    LUA_TNUMBER,
    LUA_TINTEGER,

    // When vector components are 'float', vector fits into the TValue
#if LUA_VECTOR_DOUBLE == 0
    LUA_TVECTOR,
#endif

    LUA_TSTRING, // all types above this must be value types, all types below this must be GC types - see iscollectable

    LUA_TTABLE,
    LUA_TFUNCTION,
    LUA_TUSERDATA,
    LUA_TTHREAD,
    LUA_TBUFFER,
    LUA_TCLASS,
    LUA_TOBJECT,

    // When vector components are 'double', it is a GCObject with heap-allocated data
#if LUA_VECTOR_DOUBLE == 1
    LUA_TVECTOR,
#endif

    // values below this line are used in GCObject tags but may never show up in TValue type tags

    // LUA_TDEADKEY is used in TKey to identify Luau table entries that have the value set to nil,
    // so that we can remove the strong reference to the key.
    LUA_TDEADKEY,

    // These values should never show up in TValue tag types.
    LUA_TPROTO,
    LUA_TUPVAL,

    // the count of all Luau types (including those that are never TValue type tags)
    LUA_T_ALL,

    // the count of TValue type tags
    LUA_T_COUNT = LUA_TDEADKEY,
};
// clang-format on

// type of numbers in Luau
typedef double lua_Number;

// type for integer functions
typedef int lua_Integer;

// unsigned integer type
typedef unsigned lua_Unsigned;

/*
** state manipulation
*/
LUA_API lua_State* lua_newstate(lua_Alloc allocator, void* ud);
LUA_API void lua_close(lua_State* L);
LUA_API lua_State* lua_newthread(lua_State* L);
LUA_API lua_State* lua_mainthread(lua_State* L);
LUA_API void lua_resetthread(lua_State* L);
LUA_API int lua_isthreadreset(lua_State* L);

/*
** basic stack manipulation
*/
LUA_API int lua_absindex(lua_State* L, int idx);
LUA_API int lua_gettop(lua_State* L);
LUA_API void lua_settop(lua_State* L, int idx);
LUA_API void lua_pushvalue(lua_State* L, int idx);
LUA_API void lua_remove(lua_State* L, int idx);
LUA_API void lua_insert(lua_State* L, int idx);
LUA_API void lua_replace(lua_State* L, int idx);
LUA_API int lua_checkstack(lua_State* L, int sz);
LUA_API void lua_rawcheckstack(lua_State* L, int sz); // allows for unlimited stack frames

LUA_API void lua_xmove(lua_State* from, lua_State* to, int n);
LUA_API void lua_xpush(lua_State* from, lua_State* to, int idx);

/*
** access functions (stack -> C)
*/

LUA_API int lua_isnumber(lua_State* L, int idx);
LUA_API int lua_isstring(lua_State* L, int idx);
LUA_API int lua_isinteger64(lua_State* L, int idx);
LUA_API int lua_iscfunction(lua_State* L, int idx);
LUA_API int lua_isLfunction(lua_State* L, int idx);
LUA_API int lua_isuserdata(lua_State* L, int idx);
LUA_API int lua_type(lua_State* L, int idx);
LUA_API const char* lua_typename(lua_State* L, int tp);

LUA_API int lua_equal(lua_State* L, int idx1, int idx2);
LUA_API int lua_rawequal(lua_State* L, int idx1, int idx2);
LUA_API int lua_lessthan(lua_State* L, int idx1, int idx2);

LUA_API double lua_tonumberx(lua_State* L, int idx, int* isnum);
LUA_API int lua_tointegerx(lua_State* L, int idx, int* isnum);
LUA_API unsigned lua_tounsignedx(lua_State* L, int idx, int* isnum);
LUA_API const LUA_VECTOR_TYPE* lua_tovector(lua_State* L, int idx);
LUA_API int lua_toboolean(lua_State* L, int idx);
LUA_API int64_t lua_tointeger64(lua_State* L, int idx, int* isinteger);
LUA_API const char* lua_tolstring(lua_State* L, int idx, size_t* len);
LUA_API const char* lua_tostringatom(lua_State* L, int idx, int* atom);
LUA_API const char* lua_tolstringatom(lua_State* L, int idx, size_t* len, int* atom);
LUA_API const char* lua_namecallatom(lua_State* L, int* atom);
LUA_API int lua_objlen(lua_State* L, int idx);
LUA_API lua_CFunction lua_tocfunction(lua_State* L, int idx);
LUA_API void* lua_tolightuserdata(lua_State* L, int idx);
LUA_API void* lua_tolightuserdatatagged(lua_State* L, int idx, int tag);
LUA_API void* lua_touserdata(lua_State* L, int idx);
LUA_API void* lua_touserdatatagged(lua_State* L, int idx, int tag);
LUA_API int lua_userdatatag(lua_State* L, int idx);
LUA_API int lua_lightuserdatatag(lua_State* L, int idx);
LUA_API lua_State* lua_tothread(lua_State* L, int idx);
LUA_API void* lua_tobuffer(lua_State* L, int idx, size_t* len);
LUA_API const void* lua_topointer(lua_State* L, int idx);

/*
** push functions (C -> stack)
*/
LUA_API void lua_pushnil(lua_State* L);
LUA_API void lua_pushnumber(lua_State* L, double n);
LUA_API void lua_pushinteger(lua_State* L, int n);
LUA_API void lua_pushinteger64(lua_State* L, int64_t n);
LUA_API void lua_pushunsigned(lua_State* L, unsigned n);
#if LUA_VECTOR_SIZE == 4
LUA_API void lua_pushvector(lua_State* L, LUA_VECTOR_TYPE x, LUA_VECTOR_TYPE y, LUA_VECTOR_TYPE z, LUA_VECTOR_TYPE w);
#else
LUA_API void lua_pushvector(lua_State* L, LUA_VECTOR_TYPE x, LUA_VECTOR_TYPE y, LUA_VECTOR_TYPE z);
#endif
LUA_API void lua_pushlstring(lua_State* L, const char* s, size_t l);
LUA_API void lua_pushstring(lua_State* L, const char* s);
LUA_API const char* lua_pushvfstring(lua_State* L, const char* fmt, va_list argp);
LUA_API LUA_PRINTF_ATTR(2, 3) const char* lua_pushfstringL(lua_State* L, const char* fmt, ...);
LUA_API void lua_pushcclosurek(lua_State* L, lua_CFunction fn, const char* debugname, int nup, lua_Continuation cont);
LUA_API void lua_pushboolean(lua_State* L, int b);
LUA_API int lua_pushthread(lua_State* L);

typedef void (*lua_Destructor)(lua_State* L, void* userdata);

LUA_API void lua_pushlightuserdatatagged(lua_State* L, void* p, int tag);
LUA_API void* lua_newuserdatatagged(lua_State* L, size_t sz, int tag);
LUA_API void* lua_newuserdatataggedwithmetatable(lua_State* L, size_t sz, int tag); // metatable fetched with lua_getuserdatametatable
LUA_API void* lua_newuserdatadtor(lua_State* L, size_t sz, lua_Destructor dtor);

LUA_API void* lua_newbuffer(lua_State* L, size_t sz);

/*
** get functions (Lua -> stack)
*/
LUA_API int lua_gettable(lua_State* L, int idx);
LUA_API int lua_getfield(lua_State* L, int idx, const char* k);
LUA_API int lua_rawgetfield(lua_State* L, int idx, const char* k);
LUA_API int lua_rawget(lua_State* L, int idx);
LUA_API int lua_rawgeti(lua_State* L, int idx, int n);
LUA_API int lua_rawgetptagged(lua_State* L, int idx, void* p, int tag);
LUA_API void lua_createtable(lua_State* L, int narr, int nrec);

LUA_API void lua_setreadonly(lua_State* L, int idx, int enabled);
LUA_API int lua_getreadonly(lua_State* L, int idx);
LUA_API void lua_setsafeenv(lua_State* L, int idx, int enabled);

LUA_API int lua_getmetatable(lua_State* L, int objindex);
LUA_API void lua_getfenv(lua_State* L, int idx);

/*
** set functions (stack -> Lua)
*/
LUA_API void lua_settable(lua_State* L, int idx);
LUA_API void lua_setfield(lua_State* L, int idx, const char* k);
LUA_API void lua_rawsetfield(lua_State* L, int idx, const char* k);
LUA_API void lua_rawset(lua_State* L, int idx);
LUA_API void lua_rawseti(lua_State* L, int idx, int n);
LUA_API void lua_rawsetptagged(lua_State* L, int idx, void* p, int tag);
LUA_API int lua_setmetatable(lua_State* L, int objindex);
LUA_API int lua_setfenv(lua_State* L, int idx);

/*
** `load' and `call' functions (load and run Luau bytecode)
*/
LUA_API int luau_load(lua_State* L, const char* chunkname, const char* data, size_t size, int env);
LUA_API void lua_call(lua_State* L, int nargs, int nresults);
LUA_API int lua_pcall(lua_State* L, int nargs, int nresults, int errfunc);
LUA_API int lua_cpcall(lua_State* L, lua_CFunction func, void* ud);

// wrapper for making calls from yieldable C functions
LUA_API int lua_callyieldable(lua_State* L, int nargs, int nresults);
LUA_API int lua_pcallyieldable(lua_State* L, int nargs, int nresults, int errfunc);

/*
** coroutine functions
*/
LUA_API int lua_yield(lua_State* L, int nresults);
LUA_API int lua_break(lua_State* L);
LUA_API int lua_resume(lua_State* L, lua_State* from, int narg);
LUA_API int lua_resumeerror(lua_State* L, lua_State* from);
LUA_API int lua_status(lua_State* L);
LUA_API int lua_isyieldable(lua_State* L);
LUA_API void* lua_getthreaddata(lua_State* L);
LUA_API void lua_setthreaddata(lua_State* L, void* data);
LUA_API int lua_costatus(lua_State* L, lua_State* co);
LUA_API int lua_hasfinalizers(lua_State* L);
LUA_API void lua_pushfinalizerfunction(lua_State* L, lua_State* co, int toclose);
LUA_API void lua_addfinalizer(lua_State* L, lua_State* co, int idx);

/*
** garbage-collection function and options
*/

enum lua_GCOp
{
    // stop and resume incremental garbage collection
    LUA_GCSTOP,
    LUA_GCRESTART,

    // run a full GC cycle; not recommended for latency sensitive applications
    LUA_GCCOLLECT,

    // return the heap size in KB and the remainder in bytes
    LUA_GCCOUNT,
    LUA_GCCOUNTB,

    // return 1 if GC is active (not stopped); note that GC may not be actively collecting even if it's running
    LUA_GCISRUNNING,

    /*
    ** perform an explicit GC step, with the step size specified in KB
    **
    ** garbage collection is handled by 'assists' that perform some amount of GC work matching pace of allocation
    ** explicit GC steps allow to perform some amount of work at custom points to offset the need for GC assists
    ** note that GC might also be paused for some duration (until bytes allocated meet the threshold)
    ** if an explicit step is performed during this pause, it will trigger the start of the next collection cycle
    */
    LUA_GCSTEP,

    /*
    ** tune GC parameters G (goal), S (step multiplier) and step size (usually best left ignored)
    **
    ** garbage collection is incremental and tries to maintain the heap size to balance memory and performance overhead
    ** this overhead is determined by G (goal) which is the ratio between total heap size and the amount of live data in it
    ** G is specified in percentages; by default G=200% which means that the heap is allowed to grow to ~2x the size of live data.
    **
    ** collector tries to collect S% of allocated bytes by interrupting the application after step size bytes were allocated.
    ** when S is too small, collector may not be able to catch up and the effective goal that can be reached will be larger.
    ** S is specified in percentages; by default S=200% which means that collector will run at ~2x the pace of allocations.
    **
    ** it is recommended to set S in the interval [100 / (G - 100), 100 + 100 / (G - 100))] with a minimum value of 150%; for example:
    ** - for G=200%, S should be in the interval [150%, 200%]
    ** - for G=150%, S should be in the interval [200%, 300%]
    ** - for G=125%, S should be in the interval [400%, 500%]
    */
    LUA_GCSETGOAL,
    LUA_GCSETSTEPMUL,
    LUA_GCSETSTEPSIZE,

    // return 1 if GC is in a paused state; making a GC step in a paused state will unpause the GC
    LUA_GCISPAUSED,
};

LUA_API int lua_gc(lua_State* L, int what, int data);

typedef const char* (*lua_CategoryName)(lua_State* L, uint8_t memcat);

// write a Luau memory dump to a FILE in JSON format
// categoryName callback, when provided, will be called to record a name associated with the category
LUA_API void lua_memorydump(lua_State* L, void* file, lua_CategoryName categoryName);

/*
** work counters: what scripts made the interpreter do since the state began (the counts only grow);
** functions marked with lua_setworkexempt (a host's own Luau) count apart, in hostops only
*/
typedef struct lua_WorkCounters
{
    uint64_t ops[256]; // instructions dispatched by the interpreter, by opcode (native code counts none)
    uint64_t allocated; // bytes of the collectable objects a script function's own instructions allocated (not its C calls)
    uint64_t ccalls; // C functions a script function called
    uint64_t tmcalls; // C functions called as metamethods of a script function's instructions
    uint64_t hostops[256]; // instructions of exempt functions
    uint64_t hostcalls; // exempt functions a script function called (directly or through a C function)
    uint64_t objects; // collectable objects a script function's own instructions allocated (tables, closures, strings, upvalues)
    uint64_t stored; // bytes of table storage they allocated (array and hash parts, a grown array's new size)
    uint64_t rehashed; // keys a script function's own instructions made a table rehash (its growth)
    int64_t hostnanos; // nanoseconds a native datatype operation charged the script (lua_workhostnanos; may be negative: a correction to what its instructions counted)
    uint64_t muted; // while above 0 the engine's Luau prices a whole call itself (lua_workmute): lua_workhostnanos charges nothing
    uint64_t vectormagnitudes; // .Magnitude reads the VM answered on a vector for a script's own instruction
    uint64_t vectorunits; // .Unit reads the same way
    uint64_t vectormethods; // method calls (NAMECALL) a script's own instruction made on a vector
    uint64_t slowimports; // GETIMPORTs of a script's instructions that looked the global up (an unsafe environment's, an unresolved import)
    uint64_t nativeops[256]; // instructions of the functions a code generator would compile (a --!native module's but its cold ones, @native ones): counted apart from ops
    uint64_t numtostr; // numbers a script's work turned into strings (concatenation, tostring)
    uint64_t sortcompares; // comparisons table.sort made for a script
    uint64_t sortcalls; // calls table.sort made to a script's comparison function
    uint64_t createitems; // elements table.create filled for a script
    uint64_t cloneitems; // array and hash slots table.clone copied for a script
    uint64_t concatitems; // items table.concat joined for a script
    uint64_t splitparts; // parts string.split made for a script
    uint64_t formats; // string.format calls for a script (an interpolated string is one)
    uint64_t formatspecs; // the % specifiers they formatted
    uint64_t newstrings; // strings a script's work made that the VM did not hold already (an equal string alive is reused)
    uint64_t coldtables; // tables a script's work touched first in a frame (lua_coldframe), made before the frame began
    uint64_t coldclosures; // Luau closures a script's work called first in a frame, made before it
    uint64_t coldprotos; // functions whose code a script's work ran first in a frame, loaded before it
    uint64_t coldcode; // the instructions of those functions (their code's size)
} lua_WorkCounters;

LUA_API const lua_WorkCounters* lua_workcounters(lua_State* L);
// marks the Luau function at idx and every function defined in it as the host's own (not counted as script work)
LUA_API void lua_setworkexempt(lua_State* L, int idx);
// counts host calls for the script below the native function that asks, as if it had called exempt functions (a datatype operation served natively)
LUA_API void lua_workhostcalls(lua_State* L, int calls);
// charges nanoseconds to the script below the native function that asks (a datatype operation priced from Studio's own cost; negative when
// the instructions that called it already count more than Studio spent on the whole operation)
LUA_API void lua_workhostnanos(lua_State* L, int nanos);
// on: one more level of muting; off: one less (the engine's Luau prices a whole call itself, as Scheduler.muted does for the host's charges)
LUA_API void lua_workmute(lua_State* L, int on);
// the clock the garbage collector paces itself by, in seconds (it measures the allocation rate and the mark phase's length with it to
// set the next cycle's trigger): the wall clock (lua_clock) by default, so where it steps depends on how fast the host ran; a host
// that must collect at the same points in every run gives it a clock of the work done instead. NULL restores the wall clock.
LUA_API void lua_setgcclock(lua_State* L, double (*clock)(void* ud), void* ud);

// cold touches (the work counters' cold*): a client runs a frame's engine work between two frames' scripts, so a table, closure or
// function a frame's script work touches first finds it out of the CPU's caches. On, the VM marks each table, Luau closure and
// function its instructions touch, in the object's header (a bit no other use has; nothing a script reads changes), and counts the
// first touch in a frame of one made before the frame; lua_coldframe begins a frame (every object is cold again). Off (the default),
// a touch only sets the mark once. Pages of collected objects are returned to the allocator at the next frame while it is on.
LUA_API void lua_coldtracking(lua_State* L, int on);
LUA_API void lua_coldframe(lua_State* L);
// cold calls: while cold touches are tracked, a script's first call in a frame of a C function and of a function of the host's own
// Luau (work exempt), made before the frame, is counted in these slots of the work counters' hostops, which no opcode reaches (the
// counters keep their size: the state's size counts in totalbytes)
#define LUA_WORK_COLDCFUNCS 255
#define LUA_WORK_COLDHOSTFUNCS 254
// and of those functions a script's work ran first in a frame (coldprotos), the ones whose code did not run in the frame before
#define LUA_WORK_RAREPROTOS 253
// work that grows with a library or engine function's input, counted for a script in more slots of hostops no opcode reaches:
#define LUA_WORK_SCANNED 252 // table elements a library function compared (table.find)
#define LUA_WORK_MOVED 251 // table elements or values a library function moved, copied, cleared or pushed (table.insert and table.remove at a position, table.move, table.unpack, table.pack, table.clear)
#define LUA_WORK_STRBYTES 250 // bytes of the strings a script's work made or looked up: built, hashed and compared (concatenation, the string library, the engine's strings)
#define LUA_WORK_STRSCANNED 249 // bytes a library function read from a string without making one (a plain string.find or gsub)
#define LUA_WORK_MATCHSTEPS 248 // steps of the pattern matcher: each pattern item tried at a position (string.find / match / gmatch / gsub with a pattern)
#define LUA_WORK_BUFBYTES 247 // bytes a buffer function zeroed, filled, copied or wrote
#define LUA_WORK_ALLOCBYTES 246 // bytes of every object and table part a script's work allocated: its instructions' and those of the library and engine calls it made
#define LUA_WORK_GCOBJECTS 245 // objects a script's work allocated, its instructions' and its calls' alike: what the collector sweeps (a host prices it apart)
#define LUA_WORK_LIBOBJECTS 244 // objects library and engine calls allocated for a script (its own instructions' are `objects`)
#define LUA_WORK_CASEBYTES 243 // bytes string.upper and string.lower converted
#define LUA_WORK_UTF8BYTES 242 // bytes a utf8 function decoded or stepped over (utf8.len, codepoint, offset, codes)
#define LUA_WORK_UTF8CHARS 241 // codepoints utf8.char encoded
#define LUA_WORK_STRCHARS 240 // characters string.char made from its arguments
#define LUA_WORK_BYTEVALUES 239 // values string.byte pushed
#define LUA_WORK_BUFFERS 238 // buffers made (buffer.create, buffer.fromstring)
#define LUA_WORK_MATCHCLASSES 237 // steps of the pattern matcher that tested a character class (%d, %a, [...]); the others are LUA_WORK_MATCHSTEPS
#define LUA_WORK_HASHREHASHED 236 // of the keys a script's table growth rehashed (the counters' `rehashed`), those it put in a hash part
#define LUA_WORK_LARGEBLOCKS 235 // allocations of a script's work too large for Luau's own pages (over 1,024 bytes: lmem.cpp kMaxSmallSizeUsed), each one the host's allocator makes
#define LUA_WORK_BUFFRESH 234 // bytes of a new buffer too large for Luau's own pages (over 1,024 bytes): fresh memory its zeroing touches first
#define LUA_WORK_BUFBIGCOPY 233 // bytes one buffer.copy, buffer.fill or buffer.writestring moved, of a call that moved over 64 KB (past a core's nearest caches)
// the first of these slots: a profile books each slot from here up at every step, as it books the scalar counters
#define LUA_WORK_FIRSTSLOT 233
// marks `f` as one of the host's own C functions (process-wide): an object a host function makes for a script is part of the
// operation the host prices itself, so the library counts above (LUA_WORK_LIBOBJECTS, ALLOCBYTES, GCOBJECTS) leave it out
LUA_API void lua_sethostcfunction(lua_CFunction f);
// marks `count` functions at once, and `trampoline` (the one C function a host's closures made from Rust share), taking the lock once
LUA_API void lua_sethostcfunctions(const lua_CFunction* functions, int count, lua_CFunction trampoline);

// the names a host's typeof gives its own values, for the builtin typeof (a script's fast call) to give them too: the table at
// `idx` maps a table or a userdata to its name, looked up before the value's own type name, and `vectorname` (NULL: none) names
// every vector. The table is held for the collector (a weak-keyed one keeps its weak keys).
LUA_API void lua_settypeofnames(lua_State* L, int idx, const char* vectorname);

/*
** profiling (lprofile.cpp): a host's profile of this thread's VM. Off, the interpreter pays one thread-local load per entry,
** and the VM one test of it where C allocates or rehashes, at a table's first touch in a frame, a chunk's load and a caught
** error (lprofile.h lists them).
** While a profile steps, every thread runs the interpreter's single-step variant: each instruction it dispatches is counted,
** what the work counters (priced by `prices`, one price per 8-byte slot of lua_WorkCounters) and the host's own charges
** (`*hostwork + *hostdrained`) moved since the previous instruction is booked to that instruction, a calling-context tree
** is kept (a C function a CALL reaches is a node of its own there, with what was booked until it returned), and the
** instructions with an inline cache and the builtin calls record what they did. Native code keeps running natively, unseen.
*/
#define LUA_PROFILE_EXEMPT 1 // a host's own function (lua_setworkexempt)
#define LUA_PROFILE_NATIVE 2 // compiled to native code

// what an instruction with an inline cache did, as the interpreter did it (lua_ProfileCacheSite outcomes)
#define LUA_PROFILE_HIT 0 // the cached slot served it (an import: resolved at load, in a safe environment)
#define LUA_PROFILE_REPREDICTED 1 // the key was found elsewhere in the table (a method: in its first __index table): the slot is repatched
#define LUA_PROFILE_ABSENT 2 // the key is absent and nothing else was looked at: it reads nil (an import: not resolved at load, looked up)
#define LUA_PROFILE_INSERTED 3 // a new key was inserted into the table
#define LUA_PROFILE_REHASHED 4 // a new key was inserted, and the table rehashed to fit it
#define LUA_PROFILE_METATABLE 5 // __index / __newindex tables were walked (lua_ProfileCacheSite depths)
#define LUA_PROFILE_METAFUNCTION 6 // an __index / __newindex function was called
#define LUA_PROFILE_UNSAFEENV 7 // the environment is unsafe (getfenv / setfenv): the import is looked up every time
#define LUA_PROFILE_NOTTABLE 8 // the value is not a table: its type's own metamethod served it (lua_ProfileCacheSite handlers)
#define LUA_PROFILE_READONLY 9 // a store into a read-only table (an error)
#define LUA_PROFILE_CACHES 10
#define LUA_PROFILE_DEPTHS 4 // METATABLE outcomes by the __index / __newindex tables consulted: 1, 2, 3, 4 or more
#define LUA_PROFILE_HANDLERS 4 // the types whose metamethods served NOTTABLE outcomes: three, and the rest (-2) in the last

// an instruction with an inline cache that ran (GETGLOBAL, SETGLOBAL, GETIMPORT, GETTABLEKS, SETTABLEKS, NAMECALL and their
// userdata forms GETUDATAKS, SETUDATAKS, NAMECALLUDATA)
typedef struct lua_ProfileCacheSite
{
    int pc;
    int op; // the opcode as it ran: a userdata form the interpreter rewrote to the table access it fell back to is that access
    int key; // the key it uses (lua_profilename; -1 for none): a table key, a method, a global, an import's path (math.floor)
    int phase; // the phase it ran in (lua_profilephase)
    int node; // the call-tree node it ran in (lua_profilenodes' index): an instruction has a record for each node and phase it ran in
    double charged; // what the host's prices charged the instruction past its dispatch there (a rehash's keys and storage, a looked-up import, a metamethod's engine price): part of its function's work
    double firsttouch; // of `charged`, the first touch in a frame of the table it reaches (its operand's, the environment's for a global), which any change to it still pays
    double presized; // its inserts' rehashes: what allocating the tables' storage at the size each rehash made, up front, would have charged beyond the storage they had (the host's price for table storage)
    uint64_t outcomes[LUA_PROFILE_CACHES];
    uint64_t depths[LUA_PROFILE_DEPTHS];
    int handlers[LUA_PROFILE_HANDLERS]; // lua_profilename ids (-1: none, -2: other types)
    uint64_t handlercounts[LUA_PROFILE_HANDLERS];
} lua_ProfileCacheSite;

// why a builtin's fast path declined a call (lua_ProfileBuiltinSite declines): the first of its conditions that failed
#define LUA_PROFILE_DECLINE_ARGS 0 // the argument count is one the fast path does not take
#define LUA_PROFILE_DECLINE_TYPES 1 // an argument's type is one it does not take
#define LUA_PROFILE_DECLINE_RESULTS 2 // the call asks for results it does not give (several; a fixed count of a vararg one)
#define LUA_PROFILE_DECLINE_GCDEBT 3 // a collector step was owed: the fast path cannot run the collector
#define LUA_PROFILE_DECLINE_UNSAFEENV 4 // the environment is unsafe (getfenv / setfenv): no builtin takes its fast path there
#define LUA_PROFILE_DECLINE_VALUES 5 // the argument values are out of its range (every condition listed for it held)
#define LUA_PROFILE_DECLINES 6

// a builtin call (the FASTCALL family) that ran
typedef struct lua_ProfileBuiltinSite
{
    int pc;
    int builtin; // its LBF_* id (lua_profilebuiltinname)
    int phase; // the phase it ran in (lua_profilephase)
    int node; // the call-tree node it ran in (lua_profilenodes' index): an instruction has a record for each node and phase it ran in
    double charged; // what the host's prices charged its declined calls: their fast call's dispatch, their fallback's instructions and the function those reached (a C function's work and its cold first call, the host's own Luau's)
    double fastequivalent; // what they would charge the same calls on the fast path: the fast call's dispatch and the work the builtin counts there (a new string, a number made a string, the objects and table storage it allocates, the keys its tables' rehashes move), which both paths pay
    uint64_t fast; // calls its fast path served
    uint64_t declines[LUA_PROFILE_DECLINES]; // calls that went the full way (the CALL after it), by reason
} lua_ProfileBuiltinSite;

// a copy of a function Luau's compiler inlined into another (-O2): the function, its instructions there, and the calls it
// stood for (the times its frame stepped into it from outside it)
typedef struct lua_ProfileInline
{
    int function; // the inlined function, a lua_ProfileFunction id
    int firstpc; // its first and last instruction in the function it was inlined into
    int lastpc;
    int parent; // the copy it sits in (an index into the same inlines; -1 for none), when the copied function had it inlined too
    uint64_t entries;
} lua_ProfileInline;

// a function's own lines in one phase (lua_profilephase): dispatches, work and cold work per instruction
typedef struct lua_ProfilePhaseLines
{
    int phase;
    const uint64_t* counts;
    const double* work;
    const double* cold;
} lua_ProfilePhaseLines;

typedef struct lua_ProfileFunction
{
    const char* source; // the chunk's name as lua_getinfo's short_src gives it
    const char* name; // "" when anonymous
    int linedefined;
    int flags; // LUA_PROFILE_EXEMPT, LUA_PROFILE_NATIVE
    int sizecode;
    const uint32_t* code; // its instructions as they ran (a rewritten opcode as it ran; as first seen until it runs)
    const int* lines; // the line of each instruction
    const uint64_t* counts; // dispatches per instruction (NULL without stepping)
    const double* work; // what was booked to each instruction, in the prices' unit
    int ncaches; // its instructions with an inline cache that ran
    const lua_ProfileCacheSite* caches;
    int nbuiltins; // its builtin calls that ran
    const lua_ProfileBuiltinSite* builtins;
    uint64_t nativeentries; // entries into its native code
    double nativework; // booked while its native code ran
    int ninlines; // copies of other functions inlined into it (while stepping)
    const lua_ProfileInline* inlines;
    // what of `work` was of each kind lua_profilesplit names (LUA_PROFILE_SPLITS a instruction, NULL without), and of the cold
    // charges booked to each instruction, those for objects nothing but this function touched in the frame (NULL without)
    const double* split;
    const double* exclusive;
    // its lines in each phase that ran it (sizecode entries each)
    int nphaselines;
    const lua_ProfilePhaseLines* phaselines;
    // a helper chunk of the host's binding ("=__mlua..."): its work is booked to the script instruction that called it
    int binding;
} lua_ProfileFunction;

typedef struct lua_ProfileNode
{
    int parent; // -1 for the root (0)
    int function; // >= 0 a lua_ProfileFunction; -1 the root; < -1 the C function -2 - index (lua_profilecfunction)
    int callpc; // the parent function's instruction that made the call, -1 when not known (a C or native caller)
    uint64_t calls;
    uint64_t insns; // instructions this node executed itself
    double work; // booked to this node itself
    double split[5]; // what of its work was of each kind lua_profilesplit names (LUA_PROFILE_SPLITS)
    double exclusive; // of its cold charges, those for objects nothing but its function touched in the frame
} lua_ProfileNode;

// the kinds of work a profile keeps apart in each instruction's and node's booking, besides the total (lua_profilesplit): cold and
// first-use charges, the collector's, prices of a Studio-era measurement, borrowed prices, and the engine's dispatch of a handler
// or resumption of a thread
#define LUA_PROFILE_SPLITS 5
#define LUA_PROFILE_COLD 0
#define LUA_PROFILE_COLLECTOR 1
#define LUA_PROFILE_STUDIO 2
#define LUA_PROFILE_DEFAULTED 3
#define LUA_PROFILE_DISPATCH 4
// the kinds of each slot of the work counters (`kinds`, one mask per 8-byte slot: bit k for kind k, 0 for none; a price can be of one
// kind of each axis, what it is (cold, the collector's, dispatch) and where it came from (Studio-era, defaulted)), and the host's
// running totals of its own charges of each kind (`hostkinds`, LUA_PROFILE_SPLITS of them, read like hostwork; NULL for none). Call
// after lua_profilestart.
LUA_API void lua_profilesplit(lua_State* L, const signed char* kinds, int nkinds, const double* hostkinds);

// a cold or first-use charge for an object, as a profile kept them (lua_profilecoldobjects): the object (a table by the instruction
// that made it, a C function, a function of the host's Luau), what it cost, in how many frames, and in how many of those something
// besides the function that paid also touched it (a cut of the payer leaves the charge to the other), and who paid
#define LUA_COLD_TABLE 0
#define LUA_COLD_CFUNCTION 1
#define LUA_COLD_HOSTFUNCTION 2
typedef struct lua_ProfileColdPayer
{
    int function; // a lua_ProfileFunction id
    int pc;
    double cost;
} lua_ProfileColdPayer;
typedef struct lua_ProfileColdObject
{
    int kind; // LUA_COLD_*
    int function; // a table's maker (-1: made before the profile) or a host function: a lua_ProfileFunction id; a C function: a lua_profilecfunction index
    int pc; // the instruction that made a table, -1 for the others
    double cost;
    uint64_t frames;
    uint64_t sharedframes;
    int npayers;
    const lua_ProfileColdPayer* payers;
} lua_ProfileColdObject;
LUA_API int lua_profilecoldobjects(lua_State* L, void (*each)(void* ud, const lua_ProfileColdObject* o), void* ud);
// what a profile's next booking goes to until an instruction runs: `co`'s current frame (a thread about to resume: the instruction it
// yielded at), or the Luau function on top of its stack when it has not started (a handler's new thread: its first instruction). The
// host books its dispatch of a handler or a resumption of a thread to the code that pays it
LUA_API void lua_profilebooknext(lua_State* L, lua_State* co);
// the instruction the running booking goes to: its function (-1: the engine), pc and tree node, and the node's calls so far (an
// invocation's number); 0 when no profile steps
LUA_API int lua_profilecurrent(int* function, int* pc, int* node, unsigned long long* calls);
// how many times L was resumed while a profile stepped: two of its readings with a resumption between straddle a yield (0 when no
// profile steps)
LUA_API unsigned long long lua_profileresumes(lua_State* L);

// starts a profile of L's VM on this thread (0: one runs already). Without `stepping` it only names functions for lua_profilestack
LUA_API int lua_profilestart(lua_State* L, int stepping, const double* prices, int nprices, const double* hostwork, const double* hostdrained);
// stops stepping (the last instruction's booking is made); the results stay until lua_profilefree
LUA_API void lua_profilestop(lua_State* L);
LUA_API int lua_profilefunctions(lua_State* L, void (*each)(void* ud, int id, const lua_ProfileFunction* f), void* ud);
LUA_API int lua_profilenodes(lua_State* L, void (*each)(void* ud, int id, const lua_ProfileNode* n), void* ud);
// a C function's name: where the globals hold it (table.sort, print), else the method a NAMECALL looked up (Instance:GetAttribute),
// else its own debug name
LUA_API const char* lua_profilecfunction(lua_State* L, int index);
// a name a cache site's handlers give (the type whose metamethod served it)
LUA_API const char* lua_profilename(lua_State* L, int id);
// a builtin's name by its LBF_* id (math.floor), NULL for none
LUA_API const char* lua_profilebuiltinname(int id);
// told when the interpreter is entered from outside it (entered 1) and left back to it (0), while any profile runs
LUA_API void lua_setprofilevm(lua_State* L, void (*vm)(void* ud, lua_State* L, int entered), void* ud);
// the opcode of the instruction L's innermost frame is at (in an interrupt: the one about to run), -1 when that is not a Luau frame
LUA_API int lua_profileop(lua_State* L);
// the phase what follows belongs to: frames hang from a node of their own (function -1, callpc the phase; phase 0 is the root)
LUA_API void lua_profilephase(lua_State* L, int phase);
// what was charged while no instruction ran (between resumptions)
LUA_API double lua_profileengine(lua_State* L);
LUA_API void lua_profilefree(lua_State* L);
// the frames of L, innermost first: function ids as lua_ProfileNode's, and their current lines (-1 for C and native frames)
LUA_API int lua_profilestack(lua_State* L, int* functions, int* lines, int max);
// the instruction L's innermost Luau frame is at (in an interrupt: the one about to run) and its function's id; -1 for none
// (no Luau frame, or a native one)
LUA_API int lua_profilepc(lua_State* L, int* function);
// the interpreter's entries open (C entered it for each: a script's call of a C function that calls back into Luau is one more), and
// whether L's innermost frame is a C function's (an interrupt from inside one, as the string library's matcher sends)
LUA_API int lua_profileentries(lua_State* L, int* incfunction);
// whether each chunk's functions, and the copies its compiler inlined, are registered as it loads (off by default): a profile
// that names samples without stepping needs them (a function that only ever runs as copies, a sample in a copy); stepping
// registers a chunk's functions when it first runs one
LUA_API void lua_profilechunks(lua_State* L, int on);

// a host's hook around the Luau calls it samples (a running profile's; NULL turns it off): LUA_PROFILECALL_ENTER as a
// sampled call's first instruction is about to run (its frame in place, so lua_profilestack sees it on top), and
// LUA_PROFILECALL_LEAVE as it returns, as an error unwinds it in a protected call, or as its thread is ended by an error
// or reset (its frame still in place). Each
// call of a function is drawn on its own, with a chance of one in `every` (lua_setprofilecallevery; 1 until set: every
// call), except that a call inside a sampled call of its own function on the same thread is sampled too (up to 64 such
// calls a sampled call, then they are drawn again), so a recursion is read as a whole; with `every` 0 none is sampled,
// and one call after every `remeasure` calls is read around for its length alone. The hook is told the `every` its call
// was drawn with (1 for a call sampled inside its function's, 0 for a call read for its length only) and the frame's
// depth (lua_stackdepth). Calls into native code are not sampled.
#define LUA_PROFILECALL_ENTER 0
#define LUA_PROFILECALL_LEAVE 1
LUA_API void lua_setprofilecalls(lua_State* L, void (*hook)(void* ud, lua_State* L, int event, int function, int every, int depth), void* ud);
LUA_API void lua_setprofilecallevery(lua_State* L, int function, int every, int remeasure);

// what the memory an onallocate callback reports is for (lua_allocblock)
#define LUA_ALLOC_OTHER 0
#define LUA_ALLOC_OBJECT 1 // a collectable object: lua_allocblock returns its block
#define LUA_ALLOC_TABLE 2 // a table's array or hash part
#define LUA_ALLOC_THREAD 3 // a thread's stack or call frames
#define LUA_ALLOC_STRINGS 4 // the string table
#define LUA_ALLOC_PROTO 5 // a loaded function's arrays (code, constants, line info)

// during an onallocate callback: the collectable object allocated (NULL for other memory) and what the memory is for
LUA_API void* lua_allocblock(lua_State* L, int* kind);
// an object's type (LUA_T*), and for a userdata its tag, its data and its metatable's __type
LUA_API int lua_profileobject(void* block, int* tag, void** data, const char** type);
// where a userdata's data starts in its block (lua_enumheap names a userdata by its data)
LUA_API size_t lua_profileudataoffset(void);
// marks an object: the profile's `freed` hook is called when the collector frees it
LUA_API void lua_profilemark(void* block);
LUA_API void lua_setprofilefreed(lua_State* L, void (*freed)(void* ud, void* block), void* ud);
// a table rehashed to fit a new key: the table, its array and hash sizes (nodes, 0 for none) before and after, the keys it
// moved with the new one (what the work counters' `rehashed` counts: a key that is not an integer leaves the array part in
// place) and whether a script's own instruction made it
LUA_API void lua_setprofilerehash(lua_State* L,
    void (*rehash)(void* ud, lua_State* L, void* table, int oldarray, int oldnodes, int newarray, int newnodes, int keys, int script), void* ud);
// a table's shape (ptr as lua_enumheap names it: the table): its array part's slots and how many hold a value, its hash part's
// nodes (0: none) and how many hold a value; 0 when ptr is not a table
LUA_API int lua_profiletable(void* ptr, int* arrayslots, int* arrayused, int* nodes, int* nodesused);
// the bytes of a table itself, of an array slot and of a hash node
LUA_API void lua_profiletablebytes(size_t* header, size_t* slot, size_t* node);
// what the instructions with an inline cache did, summed over the profile's functions (a host's own and its tools' aside:
// lua_profiletoolchunks), by kind: global slot (GETGLOBAL, SETGLOBAL), import (GETIMPORT), method slot (NAMECALL,
// NAMECALLUDATA), table key slot (GETTABLEKS, SETTABLEKS, GETUDATAKS, SETUDATAKS), LUA_PROFILE_CACHES outcomes each; then the
// builtin calls: served by the fast path, then declined by reason (LUA_PROFILE_DECLINES); then each kind's metatable walks by
// depth (LUA_PROFILE_DEPTHS each): into out[4 * LUA_PROFILE_CACHES + 1 + LUA_PROFILE_DECLINES + 4 * LUA_PROFILE_DEPTHS]
LUA_API int lua_profilecachetotals(lua_State* L, uint64_t* out);
// the chunks a host's tools run (their names begin with one of `prefixes`): their caches and builtin calls are left out of
// lua_profilecachetotals, as the host's own functions are
LUA_API void lua_profiletoolchunks(lua_State* L, const char* const* prefixes, int n);
// every collectable object (as luaC_enumheap gives them, lgcdebug.cpp) and every reference the collector follows between
// them, with the collector's roots as edges from NULL (the main thread, the registry, the basic types' metatables, the
// fixed objects): what no edge reaches is garbage the next collection frees. Objects a sweep has yet to free go to `dead`
// (type and size; NULL: left out)
LUA_API void lua_enumheap(lua_State* L, void* context, void (*node)(void* context, void* ptr, uint8_t tt, uint8_t memcat, size_t size, const char* name),
    void (*edge)(void* context, void* from, void* to, const char* name), void (*dead)(void* context, uint8_t tt, size_t size));
// the collector's state: GCSpause 0, GCSpropagate 1, GCSpropagateagain 2, GCSatomic 3, GCSsweep 4 (lgc.h)
LUA_API int lua_gcstate(lua_State* L);

/*
** memory statistics
** all allocated bytes are attributed to the memory category of the running thread (0..LUA_MEMORY_CATEGORIES-1)
*/

LUA_API void lua_setmemcat(lua_State* L, int category);
LUA_API size_t lua_totalbytes(lua_State* L, int category);

// measure the allocation rate in bytes/sec
// returns -1 if allocation rate cannot be measured
LUA_API int64_t lua_allocationrate(lua_State* L);

/*
** miscellaneous functions
*/

LUA_API l_noret lua_error(lua_State* L);

LUA_API int lua_next(lua_State* L, int idx);
LUA_API int lua_rawiter(lua_State* L, int idx, int iter);

LUA_API void lua_concat(lua_State* L, int n);

LUA_API void lua_setpointerencodekey(lua_State* L, uint64_t a, uint64_t b, uint64_t c, uint64_t d);
LUA_API uintptr_t lua_encodepointer(lua_State* L, uintptr_t p);

LUA_API double lua_clock();

LUA_API void lua_setuserdatatag(lua_State* L, int idx, int tag);

LUA_API void lua_setuserdatadtor(lua_State* L, int tag, lua_Destructor dtor);
LUA_API lua_Destructor lua_getuserdatadtor(lua_State* L, int tag);

// Embedder GC integration APIs
//
// Luau -> embedder (lua_setuserdatamark): when the GC marks a tagged userdata,
// the registered callback fires so the embedder can mark the corresponding
// native object as reachable.
//
// Embedder -> Luau (lua_setembeddergc): the GC calls this each cycle.
// Two cases:
// 1. Cycle reset (markref == NULL): clear accumulated information about marked
//      native objects.
// 2. Mark phase (markref != NULL): embedder must call markref for each new ref
//      that is reachable from a marked native object and return nonzero if it
//      made any progress. The GC propagates marks and repeatedly calls again
//      until the embedder returns zero.
//
// Together, these are used to implement a fixed-point reachability algorithm
// across both heaps.
//
// Note: because these functions are called during garbage collection, they
// must not perform any reentrant operations on the lua_State. Only truly
// read-only APIs like lua_getthreaddata are safe to call from here.
typedef void (*lua_UserdataMark)(lua_State* L, void* ud);
LUA_API void lua_setuserdatamark(lua_State* L, int tag, lua_UserdataMark markfn);
typedef void (*lua_EmbedderMark)(lua_State* L, int ref);
typedef int (*lua_EmbedderGc)(lua_State* L, lua_EmbedderMark markref);
LUA_API void lua_setembeddergc(lua_State* L, lua_EmbedderGc fn);

// APIs for interacting with embedder-managed weak references.
//
// Unlike lua_ref, these do not prevent collection on their own; the embedder
// must call markref(L, ref) inside lua_EmbedderGc to keep them alive. If the
// lua_getweakref API returns nil when the embedder expects the object to be
// alive, it indicates a bug in the embedder's logic.
//
// Bugs in the embedder's marking logic can manifest in two ways:
// - the native object owning a ref was not marked (Luau -> embedder), or
// - if the native object was marked, the ref was not marked (embedder -> Luau).
//
// Otherwise, a non-marking-related bug could simply be that the embedder is
// trying to access a ref that was correctly collected. Specifically, if a
// native object that logically owns a ref is destroyed, then the embedder
// should not be trying to access that ref anymore.
//
// In any case, asserting on the return value of lua_getweakref is a good way
// for embedders to catch bugs in their own marking logic.
LUA_API int lua_weakref(lua_State* L, int idx);
LUA_API int lua_weakunref(lua_State* L, int ref);
LUA_API int lua_getweakref(lua_State* L, int ref); // returns the type of the value pushed onto the stack

// alternative access for userdata metatables
// used by lua_newuserdatataggedwithmetatable to create tagged userdata with the associated metatable assigned
LUA_API void lua_setuserdatametatable(lua_State* L, int tag);
LUA_API void lua_getuserdatametatable(lua_State* L, int tag);

// Returns the name of a userdata tag - `__type` from the metatable or "userdata" if it is not set
LUA_API const char* lua_getuserdataname(lua_State* L, int tag);

// NOTE: experimental API and is subject to breaking changes
// registration of callbacks for direct userdata __index, __newindex and __namecall access with string keys assigned with an atom
// cachedslot is initially 0 and can be set to a custom value to help with data lookup inside the userdata
// IMPORTANT: cachedslot values are shared between all userdata, callbacks function of one userdata tag has to correctly handle values set by another
typedef void (*lua_UserdataDirectAccess)(lua_State* L, void* data, int atom, uint16_t* cachedslot, int utag);
typedef int (*lua_UserdataDirectNamecall)(lua_State* L, void* data, int atom, uint16_t* cachedslot, int utag);

LUA_API int lua_registeruserdatadirectaccess(
    lua_State* L,
    int tag,
    lua_UserdataDirectAccess get,
    lua_UserdataDirectAccess set,
    lua_UserdataDirectNamecall namecall
);

/*
** Direct field API
**
** lua_registeruserdatadirectfieldget registers a per-field, per-userdata-type
** handler that is invoked directly without allocating a Luau call frame.
**
** tag:   userdata tag (0..LUA_UTAG_LIMIT-1)
** field: field name string (will be interned and pinned)
** fn:    handler — receives raw userdata data pointer and result TValue slot
*/
typedef void (*lua_UserdataDirectFieldGet)(void* ud, void* result);
LUA_API void lua_registeruserdatadirectfieldget(lua_State* L, int tag, const char* field, lua_UserdataDirectFieldGet fn);

// Helpers for writing result values from a direct field handler.
LUA_API void lua_userdatadirectfield_setnumber(void* result, double n);
#if LUA_VECTOR_SIZE == 4
LUA_API void lua_userdatadirectfield_setvector(void* result, LUA_VECTOR_TYPE x, LUA_VECTOR_TYPE y, LUA_VECTOR_TYPE z, LUA_VECTOR_TYPE w);
#else
LUA_API void lua_userdatadirectfield_setvector(void* result, LUA_VECTOR_TYPE x, LUA_VECTOR_TYPE y, LUA_VECTOR_TYPE z);
#endif
LUA_API void lua_userdatadirectfield_setboolean(void* result, int b);
LUA_API void lua_userdatadirectfield_setinteger64(void* result, int64_t n);
LUA_API void lua_userdatadirectfield_setnil(void* result);

LUA_API void lua_setlightuserdataname(lua_State* L, int tag, const char* name);
LUA_API const char* lua_getlightuserdataname(lua_State* L, int tag);

LUA_API void lua_clonefunction(lua_State* L, int idx);
LUA_API int lua_usesexport(lua_State* L, int idx);

LUA_API void lua_cleartable(lua_State* L, int idx);
LUA_API void lua_clonetable(lua_State* L, int idx);

LUA_API lua_Alloc lua_getallocf(lua_State* L, void** ud);

/*
** reference system, can be used to pin objects
*/
#define LUA_NOREF -1
#define LUA_REFNIL 0

LUA_API int lua_ref(lua_State* L, int idx);
LUA_API int lua_unref(lua_State* L, int ref);

#define lua_getref(L, ref) lua_rawgeti(L, LUA_REGISTRYINDEX, (ref))

/*
** ===============================================================
** some useful macros
** ===============================================================
*/
#define lua_tonumber(L, i) lua_tonumberx(L, i, NULL)
#define lua_tointeger(L, i) lua_tointegerx(L, i, NULL)
#define lua_tounsigned(L, i) lua_tounsignedx(L, i, NULL)

#define lua_pop(L, n) lua_settop(L, -(n)-1)

#define lua_newtable(L) lua_createtable(L, 0, 0)
#define lua_newuserdata(L, s) lua_newuserdatatagged(L, s, 0)

#define lua_strlen(L, i) lua_objlen(L, (i))

#define lua_isfunction(L, n) (lua_type(L, (n)) == LUA_TFUNCTION)
#define lua_istable(L, n) (lua_type(L, (n)) == LUA_TTABLE)
#define lua_islightuserdata(L, n) (lua_type(L, (n)) == LUA_TLIGHTUSERDATA)
#define lua_isnil(L, n) (lua_type(L, (n)) == LUA_TNIL)
#define lua_isboolean(L, n) (lua_type(L, (n)) == LUA_TBOOLEAN)
#define lua_isinteger64(L, n) (lua_type(L, (n)) == LUA_TINTEGER)
#define lua_isvector(L, n) (lua_type(L, (n)) == LUA_TVECTOR)
#define lua_isthread(L, n) (lua_type(L, (n)) == LUA_TTHREAD)
#define lua_isbuffer(L, n) (lua_type(L, (n)) == LUA_TBUFFER)
#define lua_isnone(L, n) (lua_type(L, (n)) == LUA_TNONE)
#define lua_isnoneornil(L, n) (lua_type(L, (n)) <= LUA_TNIL)
#define lua_isclass(L, n) (lua_type(L, (n)) == LUA_TCLASS)
#define lua_isobject(L, n) (lua_type(L, (n)) == LUA_TOBJECT)

#define lua_pushliteral(L, s) lua_pushlstring(L, "" s, (sizeof(s) / sizeof(char)) - 1)
#define lua_pushcfunction(L, fn, debugname) lua_pushcclosurek(L, fn, debugname, 0, NULL)
#define lua_pushcclosure(L, fn, debugname, nup) lua_pushcclosurek(L, fn, debugname, nup, NULL)
#define lua_pushlightuserdata(L, p) lua_pushlightuserdatatagged(L, p, 0)

#define lua_rawgetp(L, idx, p) lua_rawgetptagged(L, idx, p, 0)
#define lua_rawsetp(L, idx, p) lua_rawsetptagged(L, idx, p, 0)

#define lua_setglobal(L, s) lua_setfield(L, LUA_GLOBALSINDEX, (s))
#define lua_getglobal(L, s) lua_getfield(L, LUA_GLOBALSINDEX, (s))

#define lua_tostring(L, i) lua_tolstring(L, (i), NULL)

#define lua_pushfstring(L, fmt, ...) lua_pushfstringL(L, fmt, ##__VA_ARGS__)

/*
** {======================================================================
** Debug API
** =======================================================================
*/

typedef struct lua_Debug lua_Debug; // activation record

// Functions to be called by the debugger in specific events
typedef void (*lua_Hook)(lua_State* L, lua_Debug* ar);

// invoke a debug hook on a thread in break/yield state
// userdata is passed to hook through lua_Debug::userdata field
LUA_API void lua_callhook(lua_State* L, lua_Hook hook, void* userdata);

LUA_API int lua_stackdepth(lua_State* L);
LUA_API int lua_getinfo(lua_State* L, int level, const char* what, lua_Debug* ar);
LUA_API int lua_getargument(lua_State* L, int level, int n);
LUA_API const char* lua_getlocal(lua_State* L, int level, int n);
LUA_API const char* lua_setlocal(lua_State* L, int level, int n);
LUA_API const char* lua_getupvalue(lua_State* L, int funcindex, int n);
LUA_API const char* lua_setupvalue(lua_State* L, int funcindex, int n);

LUA_API int lua_hascustomexecution(lua_State* L, int level); // function has custom execution data set
LUA_API int lua_incustomexecution(lua_State* L, int level);  // function is running using custom execution

LUA_API void lua_singlestep(lua_State* L, int enabled);
LUA_API int lua_breakpoint(lua_State* L, int funcindex, int line, int enabled);

// returns 1 if execution is currently at a breakpoint; should only be called from debug callbacks
LUA_API int lua_atbreakpoint(lua_State* L);

// Warning: this function is not thread-safe since it stores the result in a shared global array! Only use for debugging.
LUA_API const char* lua_debugtrace(lua_State* L);

struct lua_Debug
{
    const char* name;      // (n)
    const char* what;      // (s) `Lua', `C', `main', `tail'
    const char* source;    // (s)
    const char* short_src; // (s)
    int linedefined;       // (s)
    int currentline;       // (l)
    int protoid;           // (p) globally unique (within VM) proto id; 0 for C functions
    int bytecodeid;        // (p) proto index within its bytecode module; -1 for C functions
    unsigned char nupvals; // (u) number of upvalues
    unsigned char nparams; // (a) number of parameters
    char isvararg;         // (a)
    void* userdata; // only valid in lua_callhook

    char ssbuf[LUA_IDSIZE];
};

typedef void (*lua_Coverage)(void* context, const char* function, int linedefined, int depth, const int* hits, size_t size);

LUA_API void lua_getcoverage(lua_State* L, int funcindex, void* context, lua_Coverage callback);

typedef void (*lua_CounterFunction)(void* context, const char* function, int linedefined);
typedef void (*lua_CounterValue)(void* context, int kind, int line, uint64_t hits);

// Unlike 'lua_getcoverage', counters are customizable in ways which prevent merging them together
// 'lua_getcounters' will visit the specified function and all nested functions
// 'functionvisit' is called first to establish a function, then multiple calls of 'countervisit' are made for each counter in that function
LUA_API void lua_getcounters(lua_State* L, int funcindex, void* context, lua_CounterFunction functionvisit, lua_CounterValue countervisit);

// }======================================================================

/* Callbacks that can be used to reconfigure behavior of the VM dynamically.
 * These are shared between all coroutines.
 *
 * Note: interrupt is safe to set from an arbitrary thread but all other callbacks
 * can only be changed when the VM is not running any code */
struct lua_Callbacks
{
    void* userdata; // arbitrary userdata pointer that is never overwritten by Luau

    void (*interrupt)(lua_State* L, int gc);  // gets called at safepoints (loop back edges, call/ret, gc) if set
    void (*panic)(lua_State* L, int errcode); // gets called when an unprotected error is raised (if longjmp is used)

    void (*userthread)(lua_State* LP, lua_State* L); // gets called when L is created (LP == parent) or destroyed (LP == NULL)
    int16_t (*useratom)(lua_State* L, const char* s, size_t l); // gets called when a string is created to assign an atom id

    // NOTE: experimental API, requires a Debug flag to be called and is subject to breaking changes
    void (*userfinalizer)(lua_State* L, lua_State* co); // gets called before a finalizer is attached to 'co' by current thread

    void (*debugbreak)(lua_State* L, lua_Debug* ar);     // gets called when BREAK instruction is encountered
    void (*debugstep)(lua_State* L, lua_Debug* ar);      // gets called after each instruction in single step mode
    void (*debuginterrupt)(lua_State* L, lua_Debug* ar); // gets called when thread execution is interrupted by break in another thread
    void (*debugprotectederror)(lua_State* L);           // gets called when protected call results in an error

    // gets called after a heap object (or array) is allocated
    void (*onallocate)(lua_State* L, void* block, size_t osize, size_t nsize, uint8_t memcat, int tt, int tag);

    void (*preresume)(lua_State* L);  // gets called before lua_resume runs a (co)routine
    void (*postresume)(lua_State* L); // gets called after lua_resume returns (yield, return, or error)

    // gets called before a heap object (or array) is freed
    void (*onfree)(lua_State* L, void* block);
};
typedef struct lua_Callbacks lua_Callbacks;

LUA_API lua_Callbacks* lua_callbacks(lua_State* L);

// Must be called after lua_newstate and before the state creates any additional objects
// The VM makes no assumptions about the layout or structure of the caged heap
// The VM does assume that the embedder will free any memory allocated if the main lua_State the cage is associated with is closed
LUA_API void lua_setmemorycage(lua_State* L, lua_CageAlloc alloc, void* ud);

/******************************************************************************
 * Copyright (c) 2019-2023 Roblox Corporation
 * Copyright (C) 1994-2008 Lua.org, PUC-Rio.  All rights reserved.
 *
 * Permission is hereby granted, free of charge, to any person obtaining
 * a copy of this software and associated documentation files (the
 * "Software"), to deal in the Software without restriction, including
 * without limitation the rights to use, copy, modify, merge, publish,
 * distribute, sublicense, and/or sell copies of the Software, and to
 * permit persons to whom the Software is furnished to do so, subject to
 * the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
 * IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
 * CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
 * TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 * SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 ******************************************************************************/
