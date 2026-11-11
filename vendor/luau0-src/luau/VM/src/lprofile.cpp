// This file is part of the Luau programming language and is licensed under MIT License; see LICENSE.txt for details
// A host's profile of the interpreter (lua.h "profiling"; the engine's patches/luau/12-profiling.patch). While a profile steps, every thread of its
// VM runs luau_execute's single-step variant, whose dispatch calls luaP_step before each instruction:
// - it counts the instructions each function executes, by instruction;
// - it books what the work counters (lua_workcounters, priced by the host) and the host's own charges moved since the
//   previous instruction to that instruction, so a line's cost is exact and its sum is the host's clock;
// - it keeps a calling-context tree (call counts, inclusive costs, callers and callees) from the frames it sees. A C
//   function a CALL reaches gets a node of its own at that call site, named by where the globals hold it (table.sort) or by
//   the method a NAMECALL looked up (Instance:GetAttribute), with what was booked between the call and its return past the
//   CALL's own dispatch; a Luau function a C function calls counts a call each time it is entered;
// - it records what each instruction with an inline cache did, as the interpreter does it: GETGLOBAL, SETGLOBAL,
//   GETTABLEKS, SETTABLEKS and NAMECALL (and GETUDATAKS, SETUDATAKS, NAMECALLUDATA, which the loader makes of them for a
//   member name's key: run as the table access they fall back to, they are that access), GETIMPORT: the cached slot, a key
//   found elsewhere, absent, inserted (and whether the table rehashed to fit it), the metatables walked, an unsafe
//   environment, a value that is not a table (and whose metamethod served it); and for a builtin call (FASTCALL) whether its
//   fast path served it and, when not, why.
// Native code keeps running natively (its instructions are not dispatched, so not seen): a function's entries into it and
// the work booked while it ran are kept apart. Nothing here allocates on the VM's heap or changes what the interpreter
// does: the classification reads the tables and metatables before the instruction runs, with the lookups the interpreter is
// about to make (string keys only: an object key would be given a hash id, lgc.h HASHIDBIT, and move later tables' order).
#include "lprofile.h"

#include "lua.h"
#include "lstate.h"
#include "lfunc.h"
#include "ltable.h"
#include "ltm.h"
#include "lgc.h"
#include "Luau/BytecodeUtils.h"
#include "ldebug.h"
#include "lbytecode.h"
#include "Luau/BytecodeUtils.h"
#include "lmem.h"
#include "lstring.h"
#include "ludata.h"
#include "lbuffer.h"

#include <algorithm>
#include <atomic>
#include <string.h>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

// (the flags under which the collector marks the roots and references the heap walk follows: lgc.cpp)
LUAU_FASTFLAG(LuauGcTraceUdata)
LUAU_FASTFLAG(LuauFastpcall)
LUAU_FASTFLAG(LuauCoroutineFinally)

thread_local lua_ProfileState* luaP_state = NULL;
thread_local uint8_t luaP_allockind = 0;
thread_local void* luaP_allocblock = NULL;

namespace
{

// (luaV_gettable and luaV_settable give up on a longer __index / __newindex chain: lvmutils.cpp MAXTAGLOOP)
const int kMaxChain = 100;

struct ProfFunction
{
    Proto* proto = NULL;
    std::string source;
    std::string name;
    int linedefined = 0;
    int flags = 0;
    // its instructions as they ran: a GETUDATAKS / SETUDATAKS / NAMECALLUDATA the interpreter rewrote to the table access it
    // ran as is that access (as first seen until it runs)
    std::vector<uint32_t> code;
    std::vector<int> lines;
    std::vector<uint64_t> counts;
    std::vector<double> work;
    // the instructions with an inline cache and the builtin calls that ran: per instruction +k for cachesites[k - 1], -k for
    // builtinsites[k - 1], 0 for none (empty until one runs)
    std::vector<int> siteof;
    std::vector<lua_ProfileCacheSite> cachesites;
    std::vector<lua_ProfileBuiltinSite> builtinsites;
    uint64_t nativeentries = 0;
    double nativework = 0;
    // the last line its instructions carry, and its chunk (an index into lua_ProfileState::chunks)
    int lastline = 0;
    int chunk = -1;
    // copies of other functions Luau's compiler inlined into this one (-O2 compiles a small local function's body into its
    // callers, on the body's own lines): the innermost copy each instruction sits in (index + 1, 0 for none) and the
    // copies, found once the chunk's functions are known (at its load, else at its first step)
    bool inlinesfound = false;
    std::vector<int> inlineof;
    std::vector<lua_ProfileInline> inlines;
    // a host's tool runs it (lua_profiletoolchunks: 1, 0 not, -1 not known yet)
    int tool = -1;
    // the instructions a declined builtin call runs (from its fast call's next to its CALL): per instruction the fast call's pc + 1,
    // 0 for none (empty until a builtin site is made)
    std::vector<int> fallbackof;
    // each site record by (pc, call-tree node, phase): siteof's value for it (siteof holds the record the last step used)
    std::unordered_map<uint64_t, int> sitekeys;
    // what of each instruction's work was of each kind (LUA_PROFILE_SPLITS an instruction), and of its cold charges those for objects
    // nothing else touched in their frame (lua_profilesplit; made at the first booking)
    std::vector<double> split;
    std::vector<double> exclusive;
    // each phase's own counts, work and cold work per instruction (lines per phase), the last one used first
    std::vector<lua_ProfilePhaseLines> phaselines;
    std::vector<std::vector<uint64_t>> phasecounts;
    std::vector<std::vector<double>> phasework;
    std::vector<std::vector<double>> phasecold;
    int lastphaseline = -1;
    // a helper chunk of the host's binding (mlua's "=__mlua..."): its instructions are a script's but its work is booked to the
    // script instruction that called it, as an engine call's is
    bool binding = false;
};

// a cold charge for an object in the frame now (luaP_cold): who paid it (function, instruction, tree node) and the object's name for
// the totals (a table by its maker, a C function, a host function), and whether anything else touched it since
struct ColdRecord
{
    int kind;
    int payer;
    int pc;
    int node;
    double cost;
    bool shared;
    int sitefunction;
    int sitepc;
};

// an object's cold charges over the profile (lua_profilecoldobjects)
struct ColdTotal
{
    int kind = 0;
    int function = -1;
    int pc = -1;
    double cost = 0;
    uint64_t frames = 0;
    uint64_t sharedframes = 0;
    std::unordered_map<uint64_t, double> payers;
    std::vector<lua_ProfileColdPayer> listed;
};

// the functions of one chunk the profile registered, and the innermost of them that carries each line
struct ProfChunk
{
    std::vector<int> functions;
    std::vector<int> owner;
    bool dirty = true;
};

struct ProfCFunction
{
    int name;
};

struct ProfNode
{
    int parent;
    int function;
    int callpc;
    uint64_t calls;
    uint64_t insns;
    double work;
    double split[LUA_PROFILE_SPLITS];
    double exclusive;
};

struct ProfFrame
{
    int depth;
    Closure* cl;
    int node;
    // its RETURN ran, or C entered it again for a new call: the next step there is a new call
    bool returning;
    // the instruction this frame stepped last (kept for a function with inlined copies, to tell an entry into one)
    int lastpc;
};

// an entry of the interpreter while a profile exists: where its luau_execute frame sits on the C stack (one an error unwound
// lies deeper than the frame that caught the error) and the C function it came from (its node at the call site that reached
// it; -1: none), whose own work is what runs once the entry is left until the next step
struct ProfEntry
{
    uintptr_t sp;
    int from;
};

struct NodeKey
{
    int parent;
    int function;
    int callpc;

    bool operator==(const NodeKey& other) const
    {
        return parent == other.parent && function == other.function && callpc == other.callpc;
    }
};

struct NodeKeyHash
{
    size_t operator()(const NodeKey& key) const
    {
        uint64_t h = uint64_t(uint32_t(key.parent)) * 0x9e3779b97f4a7c15ull;
        h ^= uint64_t(uint32_t(key.function)) * 0xc2b2ae3d27d4eb4full;
        h ^= uint64_t(uint32_t(key.callpc)) * 0x165667b19e3779f9ull;
        return size_t(h ^ (h >> 29));
    }
};

// a C function by its code and the name it is listed under (one function can be reached under several: an Instance's
// __namecall serves every method)
struct CFunctionKey
{
    lua_CFunction f;
    int name;

    bool operator==(const CFunctionKey& other) const
    {
        return f == other.f && name == other.name;
    }
};

struct CFunctionKeyHash
{
    size_t operator()(const CFunctionKey& key) const
    {
        return std::hash<const void*>()((const void*)key.f) ^ (size_t(uint32_t(key.name)) * 0x9e3779b97f4a7c15ull);
    }
};

// a NAMECALL whose CALL is still to come in its frame: the C function that CALL reaches is listed under its method
struct NameCall
{
    lua_State* L;
    CallInfo* ci;
    int reg;
    int name; // "<type>:<method>", a name id
};

// a method's name for a value's type: (type, method) -> name id, the texts kept to check a hit (a string can be freed and its
// address reused)
struct MethodName
{
    std::string type;
    std::string method;
    int name;
};

struct PairHash
{
    size_t operator()(const std::pair<const void*, const void*>& key) const
    {
        return std::hash<const void*>()(key.first) * 31 + std::hash<const void*>()(key.second);
    }
};

// an outcome the next step tells
enum
{
    PENDING_NONE = 0,
    PENDING_FAST, // a builtin call: its fast path served it unless the next instruction is the fallback's first
    PENDING_INSERT, // a new key: the table rehashed to fit it if the profile saw a rehash since
    PENDING_DIRECT, // a userdata's direct access: its slot was repredicted if the instruction patched it
};

struct Pending
{
    int kind = PENDING_NONE;
    lua_State* L = NULL;
    CallInfo* ci = NULL;
    const Instruction* next = NULL;
    const Instruction* aux = NULL;
    uint64_t before = 0;
    int function = 0;
    int pc = 0;
    int op = 0;
    int reason = 0;
    // a fast call's own dispatch, at the host's price
    double price = 0;
};

static_assert(sizeof(lua_WorkCounters) % sizeof(uint64_t) == 0, "the work counters are 8-byte slots");
const int kWorkSlots = int(sizeof(lua_WorkCounters) / sizeof(uint64_t));
const int kOpsSlot = int(offsetof(lua_WorkCounters, ops) / sizeof(uint64_t));
const int kHostOpsSlot = int(offsetof(lua_WorkCounters, hostops) / sizeof(uint64_t));
const int kNativeOpsSlot = int(offsetof(lua_WorkCounters, nativeops) / sizeof(uint64_t));
// a table's first touch in a frame, and a byte of table storage
const int kColdTablesSlot = int(offsetof(lua_WorkCounters, coldtables) / sizeof(uint64_t));
const int kStoredSlot = int(offsetof(lua_WorkCounters, stored) / sizeof(uint64_t));
// what a builtin's fast path counts of its work besides its dispatch: an object it allocates, a new string, a number made a string
const int kObjectsSlot = int(offsetof(lua_WorkCounters, objects) / sizeof(uint64_t));
const int kRehashedSlot = int(offsetof(lua_WorkCounters, rehashed) / sizeof(uint64_t));
const int kNewStringsSlot = int(offsetof(lua_WorkCounters, newstrings) / sizeof(uint64_t));
const int kNumToStrSlot = int(offsetof(lua_WorkCounters, numtostr) / sizeof(uint64_t));

// What a builtin's fast path takes (lbuiltins.cpp, the conditions before each `return -1`), to tell why it declined a call:
// the argument count, the results the call asks for, the arguments' types (the first ones, then `rest` for every one after),
// and whether it declines while a collector step is owed (before checking the arguments' types, or after)
enum
{
    R_LE1 = 0, // at most one result
    R_LE2, // at most two
    R_LE0, // none
    R_EQ0, // none, and not a vararg call either (assert)
    R_MULT, // only a vararg call (table.unpack)
    R_EQ1, // exactly one (select)
    R_BYTE, // as many as the bytes the range holds, one for a vararg call (string.byte)
};

enum
{
    B_GC = 1, // declines while a collector step is owed (it cannot run the collector)
    B_GCFIRST = 2, // ... before it looks at the arguments
    B_TOSTRING = 4, // tostring: nil, booleans, numbers, strings and integers only, the collector check for a number's string
    B_RAWLEN = 8, // rawlen: a table or a string
    B_SELECT = 16, // select: a number or "#"
    B_UNCHECKED = 32, // an argument check not listed here (the integer library): only the count and the results are told
};

struct BuiltinShape
{
    const char* name;
    int8_t minparams;
    int8_t maxparams; // -1: no limit
    uint8_t results;
    const char* types;
    char rest; // 0: any
    uint8_t flags;
};

#define BS1(name, types) {name, 1, -1, R_LE1, types, 0, 0}
#define BS2(name, types) {name, 2, -1, R_LE1, types, 0, 0}
#define BSI(name) {name, 1, -1, R_LE1, "", 0, B_UNCHECKED}

// in LBF_* order (Bytecode.h LuauBuiltinFunction)
const BuiltinShape kBuiltins[] = {
    {NULL, 0, -1, R_LE1, "", 0, 0},
    {"assert", 1, -1, R_EQ0, "", 0, 0},
    BS1("math.abs", "n"),
    BS1("math.acos", "n"),
    BS1("math.asin", "n"),
    BS2("math.atan2", "nn"),
    BS1("math.atan", "n"),
    BS1("math.ceil", "n"),
    BS1("math.cosh", "n"),
    BS1("math.cos", "n"),
    BS1("math.deg", "n"),
    BS1("math.exp", "n"),
    BS1("math.floor", "n"),
    BS2("math.fmod", "nn"),
    {"math.frexp", 1, -1, R_LE2, "n", 0, 0},
    BS2("math.ldexp", "nn"),
    BS1("math.log10", "n"),
    {"math.log", 1, -1, R_LE1, "n", 'n', 0},
    {"math.max", 2, -1, R_LE1, "nn", 'n', 0},
    {"math.min", 2, -1, R_LE1, "nn", 'n', 0},
    {"math.modf", 1, -1, R_LE2, "n", 0, 0},
    BS2("math.pow", "nn"),
    BS1("math.rad", "n"),
    BS1("math.sinh", "n"),
    BS1("math.sin", "n"),
    BS1("math.sqrt", "n"),
    BS1("math.tanh", "n"),
    BS1("math.tan", "n"),
    BS2("bit32.arshift", "nn"),
    {"bit32.band", 2, -1, R_LE1, "nn", 'n', 0},
    BS1("bit32.bnot", "n"),
    {"bit32.bor", 2, -1, R_LE1, "nn", 'n', 0},
    {"bit32.bxor", 2, -1, R_LE1, "nn", 'n', 0},
    {"bit32.btest", 2, -1, R_LE1, "nn", 'n', 0},
    {"bit32.extract", 2, -1, R_LE1, "nn", 'n', 0},
    BS2("bit32.lrotate", "nn"),
    BS2("bit32.lshift", "nn"),
    {"bit32.replace", 3, -1, R_LE1, "nnn", 'n', 0},
    BS2("bit32.rrotate", "nn"),
    BS2("bit32.rshift", "nn"),
    BS1("type", ""),
    {"string.byte", 2, -1, R_BYTE, "sn", 0, 0},
    {"string.char", 0, 7, R_LE1, "", 'n', B_GC | B_GCFIRST},
    BS1("string.len", "s"),
    BS1("typeof", ""),
    {"string.sub", 3, -1, R_LE1, "snn", 0, B_GC},
    {"math.clamp", 3, -1, R_LE1, "nnn", 0, 0},
    BS1("math.sign", "n"),
    BS1("math.round", "n"),
    {"rawset", 3, -1, R_LE1, "t", 0, 0},
    BS2("rawget", "t"),
    BS2("rawequal", ""),
    {"table.insert", 2, 2, R_LE0, "t", 0, 0},
    {"table.unpack", 1, -1, R_MULT, "t", 0, 0},
    {"Vector3.new", 2, -1, R_LE1, "nn", 'n', 0},
    BS1("bit32.countlz", "n"),
    BS1("bit32.countrz", "n"),
    {"select", 1, 1, R_EQ1, "", 0, B_SELECT},
    {"rawlen", 1, -1, R_LE1, "", 0, B_RAWLEN},
    BS2("bit32.extract", "n"),
    BS1("getmetatable", ""),
    BS2("setmetatable", "tt"),
    {"tonumber", 1, 1, R_LE1, "", 0, 0},
    {"tostring", 1, -1, R_LE1, "", 0, B_TOSTRING},
    BS1("bit32.byteswap", "n"),
    BS2("buffer.readi8", "bn"),
    BS2("buffer.readu8", "bn"),
    {"buffer.writeu8", 3, -1, R_LE0, "bnn", 0, 0},
    BS2("buffer.readi16", "bn"),
    BS2("buffer.readu16", "bn"),
    {"buffer.writeu16", 3, -1, R_LE0, "bnn", 0, 0},
    BS2("buffer.readi32", "bn"),
    BS2("buffer.readu32", "bn"),
    {"buffer.writeu32", 3, -1, R_LE0, "bnn", 0, 0},
    BS2("buffer.readf32", "bn"),
    {"buffer.writef32", 3, -1, R_LE0, "bnn", 0, 0},
    BS2("buffer.readf64", "bn"),
    {"buffer.writef64", 3, -1, R_LE0, "bnn", 0, 0},
    BS1("vector.magnitude", "v"),
    BS1("vector.normalize", "v"),
    BS2("vector.cross", "vv"),
    BS2("vector.dot", "vv"),
    BS1("vector.floor", "v"),
    BS1("vector.ceil", "v"),
    BS1("vector.abs", "v"),
    BS1("vector.sign", "v"),
    {"vector.clamp", 3, -1, R_LE1, "vvv", 0, 0},
    {"vector.min", 2, -1, R_LE1, "vv", 'v', 0},
    {"vector.max", 2, -1, R_LE1, "vv", 'v', 0},
    {"math.lerp", 3, -1, R_LE1, "nnn", 0, 0},
    {"vector.lerp", 3, -1, R_LE1, "vvn", 0, 0},
    BS1("math.isnan", "n"),
    BS1("math.isinf", "n"),
    BS1("math.isfinite", "n"),
    BSI("integer.create"),
    BSI("integer.tonumber"),
    BSI("integer.neg"),
    BSI("integer.add"),
    BSI("integer.sub"),
    BSI("integer.mul"),
    BSI("integer.div"),
    BSI("integer.min"),
    BSI("integer.max"),
    BSI("integer.rem"),
    BSI("integer.idiv"),
    BSI("integer.udiv"),
    BSI("integer.urem"),
    BSI("integer.mod"),
    BSI("integer.clamp"),
    BSI("integer.band"),
    BSI("integer.bor"),
    BSI("integer.bnot"),
    BSI("integer.bxor"),
    BSI("integer.lt"),
    BSI("integer.le"),
    BSI("integer.ult"),
    BSI("integer.ule"),
    BSI("integer.gt"),
    BSI("integer.ge"),
    BSI("integer.ugt"),
    BSI("integer.uge"),
    BSI("integer.lshift"),
    BSI("integer.rshift"),
    BSI("integer.arshift"),
    BSI("integer.lrotate"),
    BSI("integer.rrotate"),
    BSI("integer.extract"),
    BSI("integer.btest"),
    BSI("integer.countrz"),
    BSI("integer.countlz"),
    BSI("integer.bswap"),
    BS2("buffer.readinteger", "bn"),
    {"buffer.writeinteger", 3, -1, R_LE0, "bni", 0, 0},
    {"table.create", 1, -1, R_LE1, "n", 0, B_GC},
    {"table.clear", 1, -1, R_LE0, "t", 0, 0},
    BS2("buffer.readvectorf32x2", "bn"),
    BS2("buffer.readvectorf32x3", "bn"),
    {"buffer.writevectorf32x2", 3, -1, R_LE0, "bnv", 0, 0},
    {"buffer.writevectorf32x3", 3, -1, R_LE0, "bnv", 0, 0},
};

#undef BS1
#undef BS2
#undef BSI

const int kBuiltinCount = int(sizeof(kBuiltins) / sizeof(kBuiltins[0]));
static_assert(sizeof(kBuiltins) / sizeof(kBuiltins[0]) == LBF_BUFFER_WRITEVECTORF32X3 + 1, "a builtin shape for each LBF_* id, in order");

// a function's calls as the call hook samples them: each with a chance of one in `every`; with `every` 0 none, and one
// call after every `remeasure` read for its length alone (`left` of those calls still to come). `open` of its sampled
// calls have not returned (on any thread: lua_ProfileState::callopen), and `nestedleft` more calls inside the sampled
// one a recursion began with may be sampled with it
struct CallSample
{
    int every = 1;
    int remeasure = 0;
    int left = 0;
    int open = 0;
    int nestedleft = 0;
};

// the calls inside a sampled call of their own function sampled with it, at most (then they are drawn again)
const int kNestedReads = 64;

// whether a sampled call of `p` is open below `ci` on L: `ci` is a recursion's inner call
static bool insampledcall(lua_State* L, CallInfo* ci, Proto* p)
{
    for (CallInfo* c = ci - 1; c >= L->base_ci; --c)
        if ((c->flags & LUA_CALLINFO_PROFILECALL) && c->p == p)
            return true;
    return false;
}

} // namespace

struct lua_ProfileState
{
    global_State* g = NULL;
    // the stepping interpreter counts instructions (else the profile only names functions for lua_profilestack)
    bool stepping = false;
    // work is booked (the host gave prices for the work counters' slots)
    bool pricing = false;
    std::vector<double> prices;
    std::vector<int> scalars;
    const double* hostwork = NULL;
    const double* hostdrained = NULL;
    double lasthost = 0;
    // Library table storage the fast path would also charge as instruction storage.
    uint64_t callstored = 0;
    lua_WorkCounters last;
    // the instruction dispatched last, and the counts its dispatch is in: ops for a script's function, nativeops for one a code
    // generator would compile (LUAU_PROTO_WORK_NATIVE), hostops for a host's own
    int lastop = -1;
    int lastbase = 0;
    // where the next booking goes: an instruction of a function (pc -1: its native code) and its tree node, or the
    // engine (target -1: what was charged while no instruction ran)
    int target = -1;
    int targetpc = 0;
    int targetnode = 0;
    // a C function's node (-1: none): what the next booking holds past the last instruction's own dispatch is its (a CALL
    // reached it, or a Luau function it called returned to it)
    int ctarget = -1;
    // the table the instruction stepped last reaches first (an inline cache's: its operand, the environment for a global; NULL for
    // none), and what its first touch in the frame was charged since the last booking (luaP_coldtouch): its site's firsttouch
    LuaTable* owntable = NULL;
    double owntouch = 0;
    double enginework = 0;
    // the frame the last step ran in
    lua_State* lastL = NULL;
    CallInfo* lastci = NULL;
    std::vector<ProfFrame>* lastframes = NULL;
    int lastnode = 0;
    // a frame was pushed for the C function the last CALL reached: the next step in the caller's frame ends it
    bool cframe = false;
    // a host's own function (lua_setworkexempt) books to the script instruction that called it
    int hosttarget = -1;
    int hosttargetpc = 0;
    int hosttargetnode = 0;
    // the node a thread's outermost frame hangs from: the root, or the current phase's node (lua_profilephase), and the phase
    int phaseroot = 0;
    // the phase what runs belongs to (lua_profilephase): a cache or builtin site keeps a record for each phase it ran in
    int phase = 0;
    // each chunk's functions are registered as it loads (lua_profilechunks)
    bool loadchunks = false;
    // the names a host's tools' chunks begin with (lua_profiletoolchunks)
    std::vector<std::string> toolprefixes;
    // the interpreter's entries while a profile exists, innermost last
    std::vector<ProfEntry> entries;
    // native code ran: any counter may move across a frame change
    bool nativeseen = false;
    // an outcome the next step tells
    Pending pending;
    // tables that rehashed since the profile began (a pending insert's outcome)
    uint64_t rehashes = 0;
    std::vector<ProfFunction> functions;
    std::vector<ProfCFunction> cfunctions;
    std::unordered_map<CFunctionKey, int, CFunctionKeyHash> cfunctionids;
    std::vector<ProfNode> nodes;
    std::unordered_map<NodeKey, int, NodeKeyHash> children;
    std::unordered_map<lua_State*, std::vector<ProfFrame>> threads;
    // each thread's resumptions while stepping (lua_profileresumes)
    std::unordered_map<lua_State*, unsigned long long> resumes;
    // while stepping, every function of a chunk is registered when the first one is seen (by the chunk's source), so a
    // function's copies inlined into another are known by the lines they carry even when it never runs itself
    std::vector<ProfChunk> chunks;
    std::unordered_map<std::string, int> chunkids;
    // whether a function's own instructions hold a copy of another's (inlined into it): (function << 32 | other) -> yes
    std::unordered_map<uint64_t, bool> inlinedin;
    // names (C functions, the types a metamethod served), by id
    std::vector<std::string> names;
    std::unordered_map<std::string, int> nameids;
    std::unordered_map<const TString*, int> stringnames;
    // C functions by where the globals hold them (bare, or in a library table: table.sort): the tables of globals read so far (a
    // script's environment, the shared globals its metatable leads to, the main thread's)
    std::unordered_set<LuaTable*> scopes;
    // (by the closure: a binding library makes every host function one C function with the host's closure in an upvalue)
    std::unordered_map<Closure*, std::pair<lua_CFunction, int>> libraries;
    std::unordered_map<std::pair<const void*, const void*>, MethodName, PairHash> methodnames;
    std::vector<NameCall> namecalls;
    // objects marked with lua_profilemark
    void (*freed)(void* ud, void* block) = NULL;
    void* freedud = NULL;
    // tables that rehashed (lua_setprofilerehash)
    void (*rehash)(void* ud, lua_State* L, void* table, int oldarray, int oldnodes, int newarray, int newnodes, int keys, int script) = NULL;
    void* rehashud = NULL;
    // the interpreter entered and left (lua_setprofilevm)
    void (*vm)(void* ud, lua_State* L, int entered) = NULL;
    void* vmud = NULL;
    // the kinds of work kept apart (lua_profilesplit): each slot's kind (-1 none), the host's running totals of its own by kind and
    // their values at the last booking
    bool splitting = false;
    std::vector<signed char> kinds;
    const double* hostkinds = NULL;
    double lasthostkinds[LUA_PROFILE_SPLITS] = {};
    // the slots booked to the function a call enters rather than the call (a closure's, a function's first touch; its code's first
    // run in a while), and the slot of a function's cold code, booked instruction by instruction (luaP_coldcode)
    std::vector<int> calleeslots;
    int coldcodeslot = -1;
    // the frame's cold charges for objects (luaP_cold), their totals over the profile, and the instruction that made each table
    std::unordered_map<GCObject*, ColdRecord> coldframe;
    std::unordered_map<uint64_t, ColdTotal> coldtotals;
    std::unordered_map<const void*, uint64_t> tablesites;
    // the host's hook around the calls it samples (lua_setprofilecalls), and each function's sampling, by function id
    void (*callhook)(void* ud, lua_State* L, int event, int function, int every, int depth) = NULL;
    void* callud = NULL;
    std::vector<CallSample> callsamples;
    uint64_t callrandom = 0x9e3779b97f4a7c15ull;
    // each thread's open sampled calls (their functions, innermost last): a thread that goes (an error ended it, it was
    // reset or freed) gives them up, so a function's count of open sampled calls stays true
    std::unordered_map<lua_State*, std::vector<int>> callopen;
};

static lua_ProfileState* stateof(lua_State* L)
{
    lua_ProfileState* s = luaP_state;
    return s && s->g == L->global ? s : NULL;
}

bool luaP_stepping(lua_State* L)
{
    lua_ProfileState* s = luaP_state;
    return s && s->stepping && s->g == L->global;
}

bool luaP_native(lua_State* L)
{
    return luaP_stepping(L);
}

static int nameid(lua_ProfileState* s, const std::string& text)
{
    auto found = s->nameids.find(text);
    if (found != s->nameids.end())
        return found->second;
    s->names.push_back(text);
    int id = int(s->names.size() - 1);
    s->nameids.emplace(text, id);
    return id;
}

static int nameid(lua_ProfileState* s, const TString* ts)
{
    auto found = s->stringnames.find(ts);
    if (found != s->stringnames.end() && s->names[found->second].size() == ts->len && memcmp(s->names[found->second].data(), getstr(ts), ts->len) == 0)
        return found->second;
    int id = nameid(s, std::string(getstr(ts), ts->len));
    s->stringnames[ts] = id;
    return id;
}

// a value's type as a profile names it: a vector by the host's name for it, a userdata by its metatable's __type (read
// without a lookup by object: lua_settypeofnames' table is keyed by objects)
static const TString* typenameof(lua_State* L, const TValue* o)
{
    if (ttisvector(o) && L->global->vectortypename)
        return L->global->vectortypename;
    return luaT_objtypenamestr(L, o);
}

static int functionof(lua_ProfileState* s, Proto* p)
{
    uintptr_t id = uintptr_t(p->userdata);
    if (id != 0 && id <= s->functions.size() && s->functions[id - 1].proto == p)
        return int(id - 1);

    ProfFunction f;
    f.proto = p;
    char buf[LUA_IDSIZE];
    f.source = p->source ? luaO_chunkid(buf, sizeof(buf), getstr(p->source), p->source->len) : "?";
    if (p->debugname)
        f.name = getstr(p->debugname);
    f.linedefined = p->linedefined;
    f.flags = ((p->flags & LUAU_PROTO_WORK_EXEMPT) ? LUA_PROFILE_EXEMPT : 0) | (p->execdata ? LUA_PROFILE_NATIVE : 0);
    f.binding = p->source && p->source->len >= 7 && memcmp(getstr(p->source), "=__mlua", 7) == 0;
    f.code.assign(p->code, p->code + p->sizecode);
    for (int pc = 0; pc < p->sizecode; pc += Luau::getOpLength(LuauOpcode(LUAU_INSN_OP(f.code[pc]))))
        f.code[pc] &= ~Instruction(LUAU_INSN_COLD);
    f.lines.resize(p->sizecode);
    f.lastline = p->linedefined;
    for (int pc = 0; pc < p->sizecode; ++pc)
    {
        f.lines[pc] = luaG_getline(p, pc);
        f.lastline = std::max(f.lastline, f.lines[pc]);
    }
    if (s->stepping)
    {
        f.counts.assign(p->sizecode, 0);
        f.work.assign(p->sizecode, 0.0);
    }
    std::string source = p->source ? std::string(getstr(p->source), p->source->len) : std::string();
    auto known = s->chunkids.find(source);
    if (known == s->chunkids.end())
    {
        known = s->chunkids.emplace(source, int(s->chunks.size())).first;
        s->chunks.emplace_back();
    }
    f.chunk = known->second;
    s->functions.push_back(std::move(f));
    p->userdata = (void*)uintptr_t(s->functions.size());
    int index = int(s->functions.size() - 1);
    ProfChunk& chunk = s->chunks[s->functions[index].chunk];
    chunk.functions.push_back(index);
    chunk.dirty = true;
    // (the functions it defines: one inlined elsewhere is known by its lines before, or without, running itself)
    for (int i = 0; i < p->sizep; ++i)
        functionof(s, p->p[i]);
    return index;
}

// the innermost registered function of a chunk carrying each line
static const std::vector<int>& chunkowners(lua_ProfileState* s, ProfChunk& chunk)
{
    if (!chunk.dirty)
        return chunk.owner;
    chunk.dirty = false;
    std::vector<int> bysize = chunk.functions;
    std::stable_sort(
        bysize.begin(),
        bysize.end(),
        [s](int a, int b)
        {
            return s->functions[a].lastline - s->functions[a].linedefined > s->functions[b].lastline - s->functions[b].linedefined;
        }
    );
    int last = 0;
    for (int id : bysize)
        last = std::max(last, s->functions[id].lastline);
    chunk.owner.assign(size_t(last) + 1, -1);
    // (larger first, so a nested function's lines end as its own)
    for (int id : bysize)
        for (int line = std::max(0, s->functions[id].linedefined); line <= s->functions[id].lastline; ++line)
            chunk.owner[line] = id;
    return chunk.owner;
}

static bool closureop(int op)
{
    return op == LOP_NEWCLOSURE || op == LOP_DUPCLOSURE || op == LOP_CAPTURE;
}

// whether `function`'s own instructions carry a line of `other`'s (other than creating its closure): `other` was inlined into
// it, so a copy of `function` inlined elsewhere holds copies of `other`
static bool inlinedinto(lua_ProfileState* s, int function, int other)
{
    uint64_t key = (uint64_t(uint32_t(function)) << 32) | uint32_t(other);
    auto known = s->inlinedin.find(key);
    if (known != s->inlinedin.end())
        return known->second;
    const ProfFunction& f = s->functions[function];
    const ProfFunction& o = s->functions[other];
    bool inlined = false;
    for (size_t pc = 0; pc < f.code.size() && !inlined; ++pc)
        inlined = f.lines[pc] >= o.linedefined && f.lines[pc] <= o.lastline && !closureop(LUAU_INSN_OP(f.code[pc]));
    s->inlinedin.emplace(key, inlined);
    return inlined;
}

// the copies of other functions inlined into `function`: runs of its instructions on another function's lines (a function
// enclosing it aside). A copy of a function inlined into the copied one sits inside its copy; the same instructions on the
// same lines again right after a copy are the next copy of that function
static void findinlines(lua_ProfileState* s, int function)
{
    ProfFunction& f = s->functions[function]; // (nothing below registers a function: the reference stays valid)
    f.inlinesfound = true;
    if (f.chunk < 0)
        return;
    const std::vector<int>& owner = chunkowners(s, s->chunks[f.chunk]);
    int n = int(f.code.size());
    std::vector<int> ownerof(n, -1);
    bool any = false;
    for (int pc = 0; pc < n; ++pc)
    {
        int line = f.lines[pc];
        if (line < 0 || size_t(line) >= owner.size())
            continue;
        int o = owner[line];
        if (o < 0 || o == function)
            continue;
        const ProfFunction& other = s->functions[o];
        if (other.linedefined <= f.linedefined && f.lastline <= other.lastline)
            continue;
        ownerof[pc] = o;
        any = true;
    }
    if (!any)
        return;
    // a closure's creation is the defining function's own, and so is the rest of the statement creating it that lies on the
    // closure's first or last line, next to it (the call it is an argument of, `function a.b()`'s store, a `return function`)
    for (int pc = 0; pc < n; ++pc)
    {
        int op = LUAU_INSN_OP(f.code[pc]);
        if (op != LOP_NEWCLOSURE && op != LOP_DUPCLOSURE)
            continue;
        ownerof[pc] = -1;
        int d = LUAU_INSN_D(f.code[pc]);
        Proto* created = NULL;
        if (op == LOP_NEWCLOSURE && d >= 0 && d < f.proto->sizep)
            created = f.proto->p[d];
        else if (op == LOP_DUPCLOSURE && d >= 0 && d < f.proto->sizek && ttisfunction(&f.proto->k[d]) && !clvalue(&f.proto->k[d])->isC)
            created = clvalue(&f.proto->k[d])->l.p;
        uintptr_t id = created ? uintptr_t(created->userdata) : 0;
        if (id == 0 || id > s->functions.size() || s->functions[id - 1].proto != created)
            continue;
        int q = int(id - 1);
        int first = s->functions[q].linedefined;
        int last = s->functions[q].lastline;
        for (int k = pc - 1; k >= 0 && ownerof[k] == q && (f.lines[k] == first || f.lines[k] == last); --k)
            ownerof[k] = -1;
        for (int k = pc + 1; k < n && (LUAU_INSN_OP(f.code[k]) == LOP_CAPTURE || (ownerof[k] == q && (f.lines[k] == first || f.lines[k] == last))); ++k)
            ownerof[k] = -1;
    }
    std::vector<int> open;
    for (int pc = 0; pc < n; ++pc)
    {
        int o = ownerof[pc];
        while (!open.empty())
        {
            int top = f.inlines[open.back()].function;
            if (o >= 0 && (o == top || inlinedinto(s, top, o)))
                break;
            f.inlines[open.back()].lastpc = pc - 1;
            open.pop_back();
        }
        if (o < 0)
            continue;
        if (!open.empty() && f.inlines[open.back()].function == o)
        {
            lua_ProfileInline& top = f.inlines[open.back()];
            int length = pc - top.firstpc;
            bool again = length > 0 && pc + length <= n;
            for (int i = 0; again && i < length; ++i)
                again = LUAU_INSN_OP(f.code[top.firstpc + i]) == LUAU_INSN_OP(f.code[pc + i]) && f.lines[top.firstpc + i] == f.lines[pc + i];
            if (!again)
                continue;
            top.lastpc = pc - 1;
            int parent = top.parent;
            open.pop_back();
            f.inlines.push_back(lua_ProfileInline{o, pc, pc, parent, 0});
            open.push_back(int(f.inlines.size() - 1));
            continue;
        }
        f.inlines.push_back(lua_ProfileInline{o, pc, pc, open.empty() ? -1 : open.back(), 0});
        open.push_back(int(f.inlines.size() - 1));
    }
    while (!open.empty())
    {
        f.inlines[open.back()].lastpc = n - 1;
        open.pop_back();
    }
    if (f.inlines.empty())
        return;
    // (a copy comes after the one it sits in, so the innermost is written last)
    f.inlineof.assign(n, 0);
    for (size_t i = 0; i < f.inlines.size(); ++i)
        for (int pc = f.inlines[i].firstpc; pc <= f.inlines[i].lastpc; ++pc)
            f.inlineof[pc] = int(i + 1);
}

// counts the copies `at` enters: the ones it sits in that the frame's previous instruction did not (a call made inside a copy
// returns to its frame's last instruction, still inside; a loop inside a copy jumps within it)
static void enterinlines(lua_ProfileState* s, ProfFunction& f, int at)
{
    ProfFrame* frame = (s->lastframes && !s->lastframes->empty()) ? &s->lastframes->back() : NULL;
    int prev = frame ? frame->lastpc : -1;
    if (frame)
        frame->lastpc = at;
    int from = (prev >= 0 && size_t(prev) < f.inlineof.size()) ? f.inlineof[prev] : 0;
    for (int copy = f.inlineof[at]; copy != 0; copy = f.inlines[copy - 1].parent + 1)
    {
        bool inside = false;
        for (int c = from; c != 0 && !inside; c = f.inlines[c - 1].parent + 1)
            inside = c == copy;
        if (inside)
            break;
        f.inlines[copy - 1].entries++;
    }
}

// C functions by where the globals hold them: bare (print), or in a table the globals hold (table.sort, Instance.new), the table's
// name taking precedence. `scope` is a table of globals (a script's environment), read with the tables its metatable's __index
// leads to (the shared globals behind it); each is read once (the engine has set its globals by the time scripts call)
static void readscope(lua_ProfileState* s, LuaTable* scope)
{
    for (int level = 0; scope && level < 4; ++level)
    {
        if (!s->scopes.insert(scope).second)
            return;
        if (scope->node != &luaH_dummynode)
        {
            for (int i = 0; i < sizenode(scope); ++i)
            {
                const LuaNode& n = scope->node[i];
                if (ttisstring(&n.key) && ttisfunction(&n.val) && clvalue(&n.val)->isC)
                    s->libraries.emplace(clvalue(&n.val), std::make_pair(clvalue(&n.val)->c.f, nameid(s, tsvalue(&n.key))));
            }
            for (int i = 0; i < sizenode(scope); ++i)
            {
                const LuaNode& n = scope->node[i];
                if (!ttisstring(&n.key) || !ttistable(&n.val) || hvalue(&n.val) == scope)
                    continue;
                LuaTable* library = hvalue(&n.val);
                if (library->node == &luaH_dummynode)
                    continue;
                std::string prefix = std::string(svalue(&n.key)) + ".";
                for (int j = 0; j < sizenode(library); ++j)
                {
                    const LuaNode& m = library->node[j];
                    if (ttisstring(&m.key) && ttisfunction(&m.val) && clvalue(&m.val)->isC)
                        s->libraries[clvalue(&m.val)] = std::make_pair(clvalue(&m.val)->c.f, nameid(s, prefix + svalue(&m.key)));
                }
            }
        }
        // (a string key's lookup: no object is given a hash id)
        const TValue* index = scope->metatable ? luaH_getstr(scope->metatable, s->g->tmname[TM_INDEX]) : NULL;
        scope = index && ttistable(index) ? hvalue(index) : NULL;
    }
}

static int cfunctionid(lua_ProfileState* s, lua_CFunction f, int name)
{
    CFunctionKey key = {f, name};
    auto found = s->cfunctionids.find(key);
    if (found != s->cfunctionids.end())
        return found->second;
    s->cfunctions.push_back(ProfCFunction{name});
    int id = int(s->cfunctions.size() - 1);
    s->cfunctionids.emplace(key, id);
    return id;
}

// a C function's name: where the globals hold it (the caller's environment `env` read first, when known), else the method a
// NAMECALL looked up (`method`, a name id, -1 for none), else its own debug name, else `?@<chunk>:<line>`, the call site that
// reached it (`caller` and `callpc`, when a Luau function called it), so unnamed functions read apart
static int cnameof(lua_ProfileState* s, Closure* cl, int method, LuaTable* env, Proto* caller = NULL, int callpc = 0)
{
    if (env)
        readscope(s, env);
    readscope(s, s->g->mainthread->gt);
    // (the closure the globals held, checked by its code: a freed closure's address can be another's)
    auto library = s->libraries.find(cl);
    if (library != s->libraries.end() && library->second.first == cl->c.f)
        return library->second.second;
    if (method >= 0)
        return method;
    if (cl->c.debugname)
        return nameid(s, cl->c.debugname);
    if (caller && caller->source)
        return nameid(s, std::string("?@") + getstr(caller->source) + ":" + std::to_string(luaG_getline(caller, callpc)));
    return nameid(s, std::string("?"));
}

// a C function a frame holds, named with the environment of the Luau function below it (its caller's globals), when there is one;
// a metamethod of a userdata (its debug name __index, __newindex, ...; the userdata its first argument) by its type too
static int cfunctionof(lua_ProfileState* s, Closure* cl, CallInfo* ci, CallInfo* base)
{
    LuaTable* env = NULL;
    if (ci && ci > base && ttisfunction((ci - 1)->func) && !clvalue((ci - 1)->func)->isC)
        env = clvalue((ci - 1)->func)->env;
    int method = -1;
    if (ci && cl->c.debugname && strncmp(getstr(cl->c.debugname), "__", 2) == 0 && ci->func + 1 < ci->top && ttisuserdata(ci->func + 1))
    {
        const TString* type = luaT_objtypenamestr(s->g->mainthread, ci->func + 1);
        method = nameid(s, std::string(getstr(type), type->len) + "." + getstr(cl->c.debugname));
    }
    // (the Luau function below it called it: its call site names an unnamed one)
    Proto* caller = NULL;
    int callpc = 0;
    if (env && !((ci - 1)->flags & LUA_CALLINFO_NATIVE))
    {
        caller = (ci - 1)->p;
        callpc = pcRel((ci - 1)->savedpc, caller);
    }
    return cfunctionid(s, cl->c.f, cnameof(s, cl, method, env, caller, callpc));
}

// "<type>:<method>" for a NAMECALL on a value of type `type`
static int methodnameof(lua_ProfileState* s, const TString* type, const TString* method)
{
    std::pair<const void*, const void*> key = {type, method};
    auto found = s->methodnames.find(key);
    if (found != s->methodnames.end() && found->second.type == getstr(type) && found->second.method == getstr(method))
        return found->second.name;
    MethodName entry = {getstr(type), getstr(method), 0};
    entry.name = nameid(s, entry.type + ":" + entry.method);
    s->methodnames[key] = entry;
    return entry.name;
}

static int child(lua_ProfileState* s, int parent, int function, int callpc)
{
    NodeKey key = {parent, function, callpc};
    auto found = s->children.find(key);
    if (found != s->children.end())
        return found->second;
    ProfNode node = {parent, function, callpc, 0, 0, 0.0, {}, 0.0};
    s->nodes.push_back(node);
    int id = int(s->nodes.size() - 1);
    s->children.emplace(key, id);
    return id;
}

// the tree node of L's current frame, from the frames this thread showed before (an error may have unwound frames, a
// C function may have called a function again at the same depth: a frame that returned or holds another function goes)
static void reconcile(lua_ProfileState* s, lua_State* L)
{
    std::vector<ProfFrame>* frames = (L == s->lastL && s->lastframes) ? s->lastframes : &s->threads[L];
    int depth = int(L->ci - L->base_ci);
    while (!frames->empty())
    {
        ProfFrame& top = frames->back();
        if (top.depth > depth || top.returning)
        {
            frames->pop_back();
            continue;
        }
        CallInfo* ci = L->base_ci + top.depth;
        if (!ttisfunction(ci->func) || clvalue(ci->func) != top.cl)
        {
            frames->pop_back();
            continue;
        }
        // Specialization can replace a prototype while keeping its closure and frame.
        Closure* cl = clvalue(ci->func);
        int function = cl->isC ? -2 - cfunctionof(s, cl, ci, L->base_ci) : functionof(s, ci->p);
        if (s->nodes[top.node].function != function)
        {
            frames->pop_back();
            continue;
        }
        break;
    }
    int parent = frames->empty() ? s->phaseroot : frames->back().node;
    for (int d = frames->empty() ? 0 : frames->back().depth + 1; d <= depth; ++d)
    {
        CallInfo* ci = L->base_ci + d;
        if (!ttisfunction(ci->func))
            continue;
        Closure* cl = clvalue(ci->func);
        int function = cl->isC ? -2 - cfunctionof(s, cl, ci, L->base_ci) : functionof(s, ci->p);
        int callpc = -1;
        if (d > 0)
        {
            CallInfo* up = ci - 1;
            if (isLua(up) && !(up->flags & LUA_CALLINFO_NATIVE))
                callpc = pcRel(up->savedpc, up->p);
        }
        int node = child(s, parent, function, callpc);
        s->nodes[node].calls++;
        frames->push_back(ProfFrame{d, cl, node, false, -1});
        parent = node;
    }
    s->lastL = L;
    s->lastci = L->ci;
    s->lastframes = frames;
    s->lastnode = frames->empty() ? 0 : frames->back().node;
    s->hosttarget = -1;
    for (auto it = frames->rbegin(); it != frames->rend(); ++it)
    {
        CallInfo* ci = L->base_ci + it->depth;
        Proto* p = ci->p;
        if (!p || (p->flags & LUAU_PROTO_WORK_EXEMPT) || s->functions[functionof(s, p)].binding)
            continue;
        s->hosttarget = functionof(s, p);
        s->hosttargetpc = (ci->flags & LUA_CALLINFO_NATIVE) ? -1 : pcRel(ci->savedpc, p);
        s->hosttargetnode = it->node;
        break;
    }
}

// a slot's price in each kind its mask names (lua_profilesplit: a slot can be of one kind of each axis, such as cold and Studio-era)
static void addmask(double* kinds, signed char mask, double moved)
{
    for (int k = 0; k < LUA_PROFILE_SPLITS; ++k)
        if (mask & (1 << k))
            kinds[k] += moved;
}

// the arrays of `f` for the profile's current phase (lines per phase)
static int phaselinesof(lua_ProfileState* s, ProfFunction& f)
{
    if (f.lastphaseline >= 0 && f.phaselines[f.lastphaseline].phase == s->phase)
        return f.lastphaseline;
    for (size_t i = 0; i < f.phaselines.size(); ++i)
        if (f.phaselines[i].phase == s->phase)
            return f.lastphaseline = int(i);
    size_t n = f.code.size();
    f.phasecounts.emplace_back(n, 0);
    f.phasework.emplace_back(n, 0.0);
    f.phasecold.emplace_back(n, 0.0);
    f.phaselines.push_back(lua_ProfilePhaseLines{s->phase, NULL, NULL, NULL});
    return f.lastphaseline = int(f.phaselines.size() - 1);
}

// a cold charge that is the function's own (its code's first run, its closure's and prototype's first touch): cutting the function saves
// it, whatever else ran in the frame
static void addexclusive(lua_ProfileState* s, int function, int pc, int node, double cost)
{
    if (cost == 0)
        return;
    if (function >= 0 && pc >= 0)
    {
        ProfFunction& f = s->functions[function];
        LUAU_ASSERT(size_t(pc) < f.code.size());
        if (f.exclusive.empty())
            f.exclusive.assign(f.code.size(), 0.0);
        f.exclusive[pc] += cost;
    }
    s->nodes[node].exclusive += cost;
}

// adds a booking's kinds to an instruction's and a node's (`kinds`: LUA_PROFILE_SPLITS seconds, or NULL for none)
static void addkinds(lua_ProfileState* s, int function, int pc, int node, const double* kinds)
{
    if (!kinds)
        return;
    if (function >= 0 && pc >= 0)
    {
        ProfFunction& f = s->functions[function];
        LUAU_ASSERT(size_t(pc) < f.code.size());
        if (f.split.empty())
            f.split.assign(f.code.size() * LUA_PROFILE_SPLITS, 0.0);
        for (int k = 0; k < LUA_PROFILE_SPLITS; ++k)
            f.split[size_t(pc) * LUA_PROFILE_SPLITS + k] += kinds[k];
        f.phasecold[phaselinesof(s, f)][pc] += kinds[LUA_PROFILE_COLD];
    }
    for (int k = 0; k < LUA_PROFILE_SPLITS; ++k)
        s->nodes[node].split[k] += kinds[k];
}

static void add(lua_ProfileState* s, double delta, const double* kinds = NULL)
{
    if (s->target >= 0)
    {
        ProfFunction& f = s->functions[s->target];
        if (s->targetpc >= 0 && size_t(s->targetpc) < f.work.size())
        {
            f.work[s->targetpc] += delta;
            f.phasework[phaselinesof(s, f)][s->targetpc] += delta;
        }
        else
            f.nativework += delta;
        s->nodes[s->targetnode].work += delta;
        addkinds(s, s->target, s->targetpc, s->targetnode, kinds);
    }
    else
    {
        s->enginework += delta;
    }
}

// the builtin site whose fallback (the instructions a declined call runs: builtinsite marks them) holds the instruction at `at`:
// its record for the call-tree node and phase the fast call ran in; NULL for none
static uint64_t sitekey(int pc, int node, int phase);
static lua_ProfileBuiltinSite* fallbacksite(ProfFunction& f, int at, int node, int phase)
{
    if (at < 0 || size_t(at) >= f.fallbackof.size() || f.fallbackof[at] == 0)
        return NULL;
    int fast = f.fallbackof[at] - 1;
    int slot = f.siteof[fast];
    if (slot < 0 && f.builtinsites[-slot - 1].node == node && f.builtinsites[-slot - 1].phase == phase)
        return &f.builtinsites[-slot - 1];
    auto found = f.sitekeys.find(sitekey(fast, node, phase));
    return found != f.sitekeys.end() && found->second < 0 ? &f.builtinsites[-found->second - 1] : NULL;
}

// books what the work counters and the host's charges moved since the last booking: the instruction dispatched since
// (one count in its function's instruction array) and what it did (allocations, calls, the host's charges); while a C
// function's node is the target, what it did past the instruction's own dispatch is that function's
// whether a builtin's fast path would count the work of `slot` too: the strings it makes, and the objects, bytes and rehashed keys of
// what it allocates, which the library counts hold for a call that was declined
static bool isFastPathCounter(int slot)
{
    if (slot == kNewStringsSlot || slot == kNumToStrSlot || slot == kObjectsSlot || slot == kStoredSlot || slot == kRehashedSlot)
        return true;

    int hostop = slot - kHostOpsSlot;
    return hostop == LUA_WORK_LIBOBJECTS || hostop == LUA_WORK_GCOBJECTS || hostop == LUA_WORK_ALLOCBYTES || hostop == LUA_WORK_LARGEBLOCKS ||
           hostop == LUA_WORK_HASHREHASHED || hostop == LUA_WORK_STRBYTES;
}

static void book(lua_ProfileState* s, lua_State* L)
{
    if (!s->pricing)
        return;
    double dispatch = 0;
    double rest = 0;
    double dispatchkinds[LUA_PROFILE_SPLITS] = {};
    double restkinds[LUA_PROFILE_SPLITS] = {};
    if (s->hostwork && s->hostdrained)
    {
        double host = *s->hostwork + *s->hostdrained;
        rest = host - s->lasthost;
        s->lasthost = host;
    }
    if (s->splitting && s->hostkinds)
    {
        for (int kind = 0; kind < LUA_PROFILE_SPLITS; ++kind)
        {
            restkinds[kind] = s->hostkinds[kind] - s->lasthostkinds[kind];
            s->lasthostkinds[kind] = s->hostkinds[kind];
        }
    }
    const uint64_t* now = reinterpret_cast<const uint64_t*>(&s->g->work);
    uint64_t* last = reinterpret_cast<uint64_t*>(&s->last);
    if (s->nativeseen && (L != s->lastL || L->ci != s->lastci))
    {
        for (int i = 0; i < LOP__COUNT; ++i)
        {
            for (int base : {kOpsSlot, kHostOpsSlot, kNativeOpsSlot})
            {
                int slot = base + i;
                if (now[slot] != last[slot])
                {
                    double moved = s->prices[slot] * double(int64_t(now[slot] - last[slot]));
                    dispatch += moved;
                    if (s->splitting)
                        addmask(dispatchkinds, s->kinds[slot], moved);
                    last[slot] = now[slot];
                }
            }
        }
    }
    else if (s->lastop >= 0)
    {
        int slot = s->lastbase + s->lastop;
        if (now[slot] != last[slot])
        {
            double moved = s->prices[slot] * double(int64_t(now[slot] - last[slot]));
            dispatch += moved;
            if (s->splitting)
                addmask(dispatchkinds, s->kinds[slot], moved);
            last[slot] = now[slot];
        }
    }
    // Both paths pay for the same results. Use the charged counters so allocations
    // inside the library include the fast path's object and byte prices too.
    double fastwork = double(s->callstored) * s->prices[kStoredSlot];
    s->callstored = 0;
    for (int slot : s->scalars)
    {
        if (now[slot] != last[slot])
        {
            double charge = s->prices[slot] * double(int64_t(now[slot] - last[slot]));
            rest += charge;
            if (s->splitting)
                addmask(restkinds, s->kinds[slot], charge);
            if (isFastPathCounter(slot))
                fastwork += charge;
            last[slot] = now[slot];
        }
    }
    if ((dispatch + rest != 0 || fastwork != 0) && s->target >= 0 && s->targetpc >= 0)
    {
        if (lua_ProfileBuiltinSite* site = fallbacksite(s->functions[s->target], s->targetpc, s->targetnode, s->phase))
        {
            site->charged += s->ctarget >= 0 ? dispatch : dispatch + rest;
            if (s->ctarget < 0)
                site->fastequivalent += fastwork;
        }
    }
    if ((rest != 0 || fastwork != 0) && s->ctarget >= 0)
    {
        const ProfNode& node = s->nodes[s->ctarget];
        int caller = node.parent >= 0 ? s->nodes[node.parent].function : -1;
        if (caller >= 0)
            if (lua_ProfileBuiltinSite* site = fallbacksite(s->functions[caller], node.callpc, node.parent, s->phase))
            {
                site->charged += rest;
                site->fastequivalent += fastwork;
            }
    }
    if (s->ctarget >= 0)
    {
        if (dispatch != 0)
            add(s, dispatch, s->splitting ? dispatchkinds : NULL);
        if (rest != 0)
        {
            s->nodes[s->ctarget].work += rest;
            addkinds(s, s->nodes[s->ctarget].function, -1, s->ctarget, s->splitting ? restkinds : NULL);
        }
    }
    else if (dispatch + rest != 0)
    {
        for (int kind = 0; kind < LUA_PROFILE_SPLITS; ++kind)
            restkinds[kind] += dispatchkinds[kind];
        add(s, dispatch + rest, s->splitting ? restkinds : NULL);
        if (rest != 0 && s->target >= 0 && s->targetpc >= 0)
        {
            ProfFunction& f = s->functions[s->target];
            if (size_t(s->targetpc) < f.siteof.size() && f.siteof[s->targetpc] > 0)
            {
                lua_ProfileCacheSite& site = f.cachesites[f.siteof[s->targetpc] - 1];
                site.charged += rest;
                site.firsttouch += s->owntouch;
            }
        }
    }
    s->owntouch = 0;
}

// books what the slots of a call's callee moved (a closure's and a function's first touch in a frame, its code's first run in a while)
// to the instruction about to run: the callee's first, not the call
static void bookcallee(lua_ProfileState* s)
{
    if (!s->pricing || s->calleeslots.empty())
        return;
    const uint64_t* now = reinterpret_cast<const uint64_t*>(&s->g->work);
    uint64_t* last = reinterpret_cast<uint64_t*>(&s->last);
    double delta = 0;
    double kinds[LUA_PROFILE_SPLITS] = {};
    for (int slot : s->calleeslots)
    {
        if (now[slot] != last[slot])
        {
            double moved = s->prices[slot] * double(int64_t(now[slot] - last[slot]));
            delta += moved;
            addmask(kinds, s->kinds[slot], moved);
            last[slot] = now[slot];
        }
    }
    if (delta != 0)
    {
        add(s, delta, kinds);
        addexclusive(s, s->target, s->targetpc, s->targetnode, kinds[LUA_PROFILE_COLD]);
    }
}

static bool slothit(LuaNode* n, const TValue* kv)
{
    return ttisstring(gkey(n)) && tsvalue(gkey(n)) == tsvalue(kv) && !ttisnil(gval(n));
}

// a site record's key: its instruction, the call-tree node and the phase it ran in
static uint64_t sitekey(int pc, int node, int phase)
{
    return (uint64_t(uint32_t(node)) << 32) | (uint64_t(uint32_t(pc) & 0xffffff) << 8) | uint64_t(uint32_t(phase) & 0xff);
}

// the site of an instruction with an inline cache, made the first time it runs in a call-tree node and phase (a record for each
// it runs in; phase -1: the record it ran with last)
static lua_ProfileCacheSite& cachesite(ProfFunction& f, int pc, int op, int key, int phase = -1, int node = 0)
{
    if (f.siteof.empty())
        f.siteof.assign(f.code.size(), 0);
    int& slot = f.siteof[pc];
    bool same = slot > 0 && (phase < 0 || (f.cachesites[slot - 1].phase == phase && f.cachesites[slot - 1].node == node));
    if (!same)
    {
        uint64_t k = sitekey(pc, phase < 0 ? 0 : node, phase < 0 ? 0 : phase);
        auto found = f.sitekeys.find(k);
        if (found != f.sitekeys.end() && found->second > 0)
        {
            slot = found->second;
        }
        else
        {
            lua_ProfileCacheSite site;
            memset(&site, 0, sizeof(site));
            site.pc = pc;
            site.key = key;
            site.phase = phase < 0 ? 0 : phase;
            site.node = phase < 0 ? 0 : node;
            for (int i = 0; i < LUA_PROFILE_HANDLERS; ++i)
                site.handlers[i] = -1;
            f.cachesites.push_back(site);
            slot = int(f.cachesites.size());
            f.sitekeys[k] = slot;
        }
    }
    lua_ProfileCacheSite& site = f.cachesites[slot - 1];
    site.op = op;
    return site;
}

// the site of a builtin call, made the first time it runs in a call-tree node and phase (a record for each it runs in)
static lua_ProfileBuiltinSite& builtinsite(ProfFunction& f, int pc, int builtin, int phase, int node)
{
    if (f.siteof.empty())
        f.siteof.assign(f.code.size(), 0);
    // (its fallback's instructions: from the fast call's next to the CALL it skips, which a declined call runs)
    if (f.siteof[pc] == 0)
    {
        if (f.fallbackof.empty())
            f.fallbackof.assign(f.code.size(), 0);
        Instruction insn = f.code[pc];
        int call = pc + 1 + int(LUAU_INSN_C(insn));
        for (int at = pc + Luau::getOpLength(LuauOpcode(LUAU_INSN_OP(insn))); at <= call && size_t(at) < f.code.size(); ++at)
            if (f.fallbackof[at] == 0)
                f.fallbackof[at] = pc + 1;
    }
    int& slot = f.siteof[pc];
    if (slot >= 0 || f.builtinsites[-slot - 1].phase != phase || f.builtinsites[-slot - 1].node != node)
    {
        uint64_t k = sitekey(pc, node, phase);
        auto found = f.sitekeys.find(k);
        if (found != f.sitekeys.end() && found->second < 0)
        {
            slot = found->second;
        }
        else
        {
            lua_ProfileBuiltinSite site;
            memset(&site, 0, sizeof(site));
            site.pc = pc;
            site.phase = phase;
            site.node = node;
            f.builtinsites.push_back(site);
            slot = -int(f.builtinsites.size());
            f.sitekeys[k] = slot;
        }
    }
    lua_ProfileBuiltinSite& site = f.builtinsites[-slot - 1];
    site.builtin = builtin;
    return site;
}

// a NOTTABLE outcome's type: one of the site's first named types, else the last slot's "others" (-2)
static void addhandler(lua_ProfileCacheSite& site, int type)
{
    for (int i = 0; i < LUA_PROFILE_HANDLERS - 1; ++i)
    {
        if (site.handlers[i] == type || site.handlers[i] == -1)
        {
            site.handlers[i] = type;
            site.handlercounts[i]++;
            return;
        }
    }
    int last = LUA_PROFILE_HANDLERS - 1;
    if (site.handlers[last] == -1 || site.handlers[last] == type)
        site.handlers[last] = type;
    else
        site.handlers[last] = -2;
    site.handlercounts[last]++;
}

// the key an instruction with an inline cache uses (a name id, -1 for none): a table key, a method, a global, an import's path
static int keyof(lua_ProfileState* s, Proto* p, const Instruction* pc, int op)
{
    TValue* k = p->k;
    int sizek = p->sizek;
    uint32_t aux = pc[1];
    switch (op)
    {
    case LOP_GETIMPORT:
    {
        // (AUX: the path's length in its top two bits, then up to three constants' indices of ten bits each)
        int count = int(aux >> 30);
        std::string path;
        for (int i = 0; i < count; ++i)
        {
            int index = int((aux >> (20 - 10 * i)) & 1023);
            if (index >= sizek || !ttisstring(&k[index]))
                return -1;
            if (i > 0)
                path += ".";
            path += svalue(&k[index]);
        }
        return path.empty() ? -1 : nameid(s, path);
    }
    case LOP_GETUDATAKS:
    case LOP_SETUDATAKS:
    case LOP_NAMECALLUDATA:
        aux = LUAU_INSN_AUX_KV16(aux);
        break;
    }
    return int(aux) < sizek && ttisstring(&k[aux]) ? nameid(s, tsvalue(&k[aux])) : -1;
}

static void outcome(ProfFunction& f, int pc, int op, int what, int depth = 0, int handler = -1)
{
    lua_ProfileCacheSite& site = cachesite(f, pc, op, -1);
    site.outcomes[what]++;
    // (the __index / __newindex tables a walk consulted)
    if (depth > 0 && what == LUA_PROFILE_METATABLE)
        site.depths[std::min(depth, LUA_PROFILE_DEPTHS) - 1]++;
    if (handler >= 0)
        addhandler(site, handler);
}

// How a get of `key` ends past the indexed table's own lookup, read as luaV_gettable walks it from that table's metatable
// `mt`: absent when it has no __index (nothing else is looked at); else the __index tables consulted (`depth`) to the one
// holding the key (`found`) or the chain's end, or an __index function
static int getchain(lua_State* L, LuaTable* mt, TString* key, int* depth, bool* found)
{
    *found = false;
    const TValue* tm = fasttm(L, mt, TM_INDEX);
    if (!tm)
        return LUA_PROFILE_ABSENT;
    for (int d = 1; d <= kMaxChain; ++d)
    {
        *depth = d;
        if (ttisfunction(tm))
            return LUA_PROFILE_METAFUNCTION;
        const TValue* next = NULL;
        if (ttistable(tm))
        {
            LuaTable* h = hvalue(tm);
            if (!ttisnil(luaH_getstr(h, key)))
            {
                *found = true;
                return LUA_PROFILE_METATABLE;
            }
            next = fasttm(L, h->metatable, TM_INDEX);
            if (!next)
                return LUA_PROFILE_METATABLE;
        }
        else
        {
            // (a value that is not a table serves through its type's __index; none is an error)
            next = luaT_gettmbyobj(L, tm, TM_INDEX);
            if (ttisnil(next))
                return LUA_PROFILE_METATABLE;
        }
        tm = next;
    }
    return LUA_PROFILE_METATABLE;
}

// How a set of `key` ends from a table's __newindex `tm` on, read as luaV_settable walks it: the tables consulted (`depth`) to
// the one the key is assigned into, or a __newindex function
static int setchain(lua_State* L, const TValue* tm, TString* key, int* depth)
{
    for (int d = 1; d <= kMaxChain; ++d)
    {
        *depth = d;
        if (ttisfunction(tm))
            return LUA_PROFILE_METAFUNCTION;
        const TValue* next = NULL;
        if (ttistable(tm))
        {
            LuaTable* h = hvalue(tm);
            if (!ttisnil(luaH_getstr(h, key)) || (next = fasttm(L, h->metatable, TM_NEWINDEX)) == NULL)
                return h->readonly ? LUA_PROFILE_READONLY : LUA_PROFILE_METATABLE;
        }
        else
        {
            next = luaT_gettmbyobj(L, tm, TM_NEWINDEX);
            if (ttisnil(next))
                return LUA_PROFILE_METATABLE;
        }
        tm = next;
    }
    return LUA_PROFILE_METATABLE;
}

static void pendinginsert(lua_ProfileState* s, lua_State* L, int function, int at, int op)
{
    s->pending = Pending();
    s->pending.kind = PENDING_INSERT;
    s->pending.L = L;
    s->pending.ci = L->ci;
    s->pending.before = s->rehashes;
    s->pending.function = function;
    s->pending.pc = at;
    s->pending.op = op;
}

// a store of `kv` into the table `h` past its cached slot, as luaV_settable does it (SETTABLEKS's slow path, SETGLOBAL's)
static void settable(lua_ProfileState* s, lua_State* L, LuaTable* h, const TValue* kv, ProfFunction& f, int function, int at, int op)
{
    TString* key = tsvalue(kv);
    const TValue* oldval = luaH_getstr(h, key);
    const TValue* tm = NULL;
    if (!ttisnil(oldval) || (tm = fasttm(L, h->metatable, TM_NEWINDEX)) == NULL)
    {
        if (h->readonly)
            return outcome(f, at, op, LUA_PROFILE_READONLY);
        // (luaH_setslot reuses the key's node when the table has one)
        if (oldval != luaO_nilobject)
            return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
        return pendinginsert(s, L, function, at, op);
    }
    int depth = 0;
    int what = setchain(L, tm, key, &depth);
    outcome(f, at, op, what, depth);
}

// GETTABLEKS, SETTABLEKS and NAMECALL on the value at B with the constant key `kv`, read before the interpreter runs it
static void tableop(lua_ProfileState* s, lua_State* L, const Instruction* pc, int op, const TValue* kv, ProfFunction& f, int function, int at)
{
    Instruction insn = *pc;
    StkId rb = L->base + LUAU_INSN_B(insn);
    TString* key = tsvalue(kv);
    switch (op)
    {
    case LOP_GETTABLEKS:
    {
        if (!ttistable(rb))
            return outcome(f, at, op, LUA_PROFILE_NOTTABLE, 0, nameid(s, typenameof(L, rb)));
        LuaTable* h = hvalue(rb);
        if (slothit(&h->node[LUAU_INSN_C(insn) & h->nodemask8], kv))
            return outcome(f, at, op, LUA_PROFILE_HIT);
        // (with no metatable the interpreter looks the key up itself, else luaV_gettable does: the same first lookup)
        if (!ttisnil(luaH_getstr(h, key)))
            return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
        if (!h->metatable)
            return outcome(f, at, op, LUA_PROFILE_ABSENT);
        int depth = 0;
        bool found = false;
        int what = getchain(L, h->metatable, key, &depth, &found);
        return outcome(f, at, op, what, depth);
    }
    case LOP_SETTABLEKS:
    {
        if (!ttistable(rb))
            return outcome(f, at, op, LUA_PROFILE_NOTTABLE, 0, nameid(s, typenameof(L, rb)));
        LuaTable* h = hvalue(rb);
        if (slothit(&h->node[LUAU_INSN_C(insn) & h->nodemask8], kv) && !h->readonly)
            return outcome(f, at, op, LUA_PROFILE_HIT);
        if (fastnotm(h->metatable, TM_NEWINDEX) && !h->readonly)
        {
            // (luaH_setstr: the key's node when the table has one, else a new key)
            if (luaH_getstr(h, key) != luaO_nilobject)
                return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
            return pendinginsert(s, L, function, at, op);
        }
        return settable(s, L, h, kv, f, function, at, op);
    }
    case LOP_NAMECALL:
    {
        if (ttistable(rb))
        {
            LuaTable* h = hvalue(rb);
            LuaNode* n = &h->node[key->hash & (sizenode(h) - 1)];
            if (slothit(n, kv))
                return outcome(f, at, op, LUA_PROFILE_HIT);
            bool chainless = gnext(n) == 0;
            const TValue* mt = NULL;
            if (chainless && (mt = fasttm(L, h->metatable, TM_INDEX)) && ttistable(mt) && slothit(&hvalue(mt)->node[LUAU_INSN_C(insn) & hvalue(mt)->nodemask8], kv))
                return outcome(f, at, op, LUA_PROFILE_HIT);
            if (!ttisnil(luaH_getstr(h, key)))
                return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
            int depth = 0;
            bool found = false;
            int what = getchain(L, h->metatable, key, &depth, &found);
            // (found in the first __index table: the slot is repatched, and the next call finds it there by the slot unless the
            // object's own node for the key heads a chain)
            if (what == LUA_PROFILE_METATABLE && found && depth == 1 && chainless)
                return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
            return outcome(f, at, op, what, depth);
        }
        LuaTable* mt = ttisuserdata(rb) ? uvalue(rb)->metatable : L->global->mt[ttype(rb)];
        const TString* type = typenameof(L, rb);
        // (the C function the CALL reaches is listed under this method)
        if (s->namecalls.size() >= 64)
            s->namecalls.erase(s->namecalls.begin(), s->namecalls.begin() + 32);
        s->namecalls.push_back(NameCall{L, L->ci, int(LUAU_INSN_A(insn)), methodnameof(s, type, key)});
        if (fasttm(L, mt, TM_NAMECALL))
            return outcome(f, at, op, LUA_PROFILE_NOTTABLE, 0, nameid(s, type));
        const TValue* tmi = fasttm(L, mt, TM_INDEX);
        if (tmi && ttistable(tmi))
        {
            LuaTable* h = hvalue(tmi);
            if (slothit(&h->node[LUAU_INSN_C(insn) & h->nodemask8], kv))
                return outcome(f, at, op, LUA_PROFILE_HIT);
            if (!ttisnil(luaH_getstr(h, key)))
                return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
            int depth = 0;
            bool found = false;
            int what = getchain(L, h->metatable, key, &depth, &found);
            // (the __index table was consulted and lacks the method: a metatable walk that found nothing)
            if (what == LUA_PROFILE_ABSENT)
                return outcome(f, at, op, LUA_PROFILE_METATABLE, 1);
            return outcome(f, at, op, what, depth + 1);
        }
        return outcome(f, at, op, LUA_PROFILE_NOTTABLE, 0, nameid(s, type));
    }
    }
}

// An instruction with an inline cache, read before the interpreter runs it
static void cacheop(lua_ProfileState* s, lua_State* L, Closure* cl, const Instruction* pc, int op, ProfFunction& f, int function, int at, int node)
{
    Instruction insn = *pc;
    TValue* k = L->ci->p->k;
    // (the site, with the key it uses, made the first time it runs in this call-tree node and phase)
    if (f.siteof.empty() || f.siteof[at] <= 0 || f.cachesites[f.siteof[at] - 1].phase != s->phase || f.cachesites[f.siteof[at] - 1].node != node)
        cachesite(f, at, op, keyof(s, L->ci->p, pc, op), s->phase, node);
    // (the table it reaches first: its first touch in the frame is told apart from what its outcome costs, luaP_coldtouch)
    if (op == LOP_GETGLOBAL || op == LOP_SETGLOBAL || op == LOP_GETIMPORT)
        s->owntable = cl->env;
    else if (ttistable(L->base + LUAU_INSN_B(insn)))
        s->owntable = hvalue(L->base + LUAU_INSN_B(insn));
    switch (op)
    {
    case LOP_GETGLOBAL:
    case LOP_SETGLOBAL:
    {
        LuaTable* h = cl->env;
        const TValue* kv = &k[pc[1]];
        TString* key = tsvalue(kv);
        if (slothit(&h->node[LUAU_INSN_C(insn) & h->nodemask8], kv) && (op == LOP_GETGLOBAL || !h->readonly))
            return outcome(f, at, op, LUA_PROFILE_HIT);
        if (op == LOP_SETGLOBAL)
            return settable(s, L, h, kv, f, function, at, op);
        if (!ttisnil(luaH_getstr(h, key)))
            return outcome(f, at, op, LUA_PROFILE_REPREDICTED);
        int depth = 0;
        bool found = false;
        int what = getchain(L, h->metatable, key, &depth, &found);
        return outcome(f, at, op, what, depth);
    }
    case LOP_GETIMPORT:
    {
        const TValue* kv = &k[LUAU_INSN_D(insn)];
        if (!ttisnil(kv) && cl->env->safeenv)
            return outcome(f, at, op, LUA_PROFILE_HIT);
        // (looked up each time: an unsafe environment's, or one not resolved at load: a global set later, or missing then)
        return outcome(f, at, op, cl->env->safeenv ? LUA_PROFILE_ABSENT : LUA_PROFILE_UNSAFEENV);
    }
    case LOP_GETUDATAKS:
    case LOP_SETUDATAKS:
    case LOP_NAMECALLUDATA:
    {
        StkId rb = L->base + LUAU_INSN_B(insn);
        if (ttisuserdata(rb) && unsigned(uvalue(rb)->tag) < unsigned(UTAG_INTERNAL_LIMIT))
        {
            const lua_UdataDirectAccessData& direct = L->global->udatadirect[uvalue(rb)->tag];
            bool served = op == LOP_GETUDATAKS   ? direct.index != NULL && !ttisnil(&direct.indextm)
                          : op == LOP_SETUDATAKS ? direct.newindex != NULL && !ttisnil(&direct.newindextm)
                                                 : direct.namecall != NULL && !ttisnil(&direct.namecalltm);
            if (served)
            {
                // the userdata's direct access serves it with the slot its AUX holds: repredicted when the access patches it
                s->pending = Pending();
                s->pending.kind = PENDING_DIRECT;
                s->pending.aux = pc + 1;
                s->pending.before = LUAU_INSN_AUX_SLOT(pc[1]);
                s->pending.function = function;
                s->pending.pc = at;
                s->pending.op = op;
                return;
            }
        }
        // the interpreter rewrites it to the table access and runs that (the loader made it for a member name's key; no direct
        // access serves this value): it is that access, and the profile's copy of the code says so
        int access = op == LOP_GETUDATAKS ? LOP_GETTABLEKS : op == LOP_SETUDATAKS ? LOP_SETTABLEKS : LOP_NAMECALL;
        f.code[at] = (f.code[at] & ~0xffu) | uint32_t(access);
        return tableop(s, L, pc, access, &k[LUAU_INSN_AUX_KV16(pc[1])], f, function, at);
    }
    default:
        return tableop(s, L, pc, op, &k[pc[1]], f, function, at);
    }
}

static bool argtype(const TValue* v, char type)
{
    switch (type)
    {
    case 'n':
        return ttisnumber(v);
    case 's':
        return ttisstring(v);
    case 't':
        return ttistable(v);
    case 'v':
        return ttisvector(v);
    case 'b':
        return ttisbuffer(v);
    case 'i':
        return ttisinteger(v);
    }
    return true;
}

// Why the builtin a FASTCALL names would decline this call (read before it runs, from its arguments as the builtin gets them):
// the first condition of lbuiltins.cpp's that fails, VALUES when every listed one holds
static int declinereason(lua_State* L, Closure* cl, const Instruction* pc, int op)
{
    Instruction insn = *pc;
    int bfid = LUAU_INSN_A(insn);
    if (bfid <= 0 || bfid >= kBuiltinCount || !kBuiltins[bfid].name)
        return LUA_PROFILE_DECLINE_VALUES;
    const BuiltinShape& shape = kBuiltins[bfid];
    StkId base = L->base;
    Instruction call = pc[1 + LUAU_INSN_C(insn)];
    StkId ra = base + LUAU_INSN_A(call);
    int nresults = LUAU_INSN_C(call) - 1;
    int nparams = 0;
    const TValue* fixed[3] = {NULL, NULL, NULL};
    switch (op)
    {
    case LOP_FASTCALL:
        nparams = LUAU_INSN_B(call) - 1;
        if (nparams == LUA_MULTRET)
            nparams = int(L->top - ra - 1);
        break;
    case LOP_FASTCALL1:
        nparams = 1;
        fixed[0] = base + LUAU_INSN_B(insn);
        break;
    case LOP_FASTCALL2:
        nparams = 2;
        fixed[0] = base + LUAU_INSN_B(insn);
        fixed[1] = base + pc[1];
        break;
    case LOP_FASTCALL2K:
        nparams = 2;
        fixed[0] = base + LUAU_INSN_B(insn);
        fixed[1] = &L->ci->p->k[pc[1]];
        break;
    case LOP_FASTCALL3:
        nparams = 3;
        fixed[0] = base + LUAU_INSN_B(insn);
        fixed[1] = base + LUAU_INSN_AUX_A(pc[1]);
        fixed[2] = base + LUAU_INSN_AUX_B(pc[1]);
        break;
    }
    auto arg = [&](int i) -> const TValue* { return op == LOP_FASTCALL ? ra + 1 + i : fixed[i]; };

    if (nparams < shape.minparams || (shape.maxparams >= 0 && nparams > shape.maxparams))
        return LUA_PROFILE_DECLINE_ARGS;
    bool results = true;
    switch (shape.results)
    {
    case R_LE1:
        results = nresults <= 1;
        break;
    case R_LE2:
        results = nresults <= 2;
        break;
    case R_LE0:
        results = nresults <= 0;
        break;
    case R_EQ0:
        results = nresults == 0;
        break;
    case R_MULT:
        results = nresults < 0;
        break;
    case R_EQ1:
        results = nresults == 1;
        break;
    }
    if (!results)
        return LUA_PROFILE_DECLINE_RESULTS;
    if ((shape.flags & B_GCFIRST) && luaC_needsGC(L))
        return LUA_PROFILE_DECLINE_GCDEBT;
    if (!(shape.flags & B_UNCHECKED))
    {
        int listed = int(strlen(shape.types));
        for (int i = 0; i < nparams && (i < listed || shape.rest); ++i)
        {
            const TValue* v = arg(i);
            if (v && !argtype(v, i < listed ? shape.types[i] : shape.rest))
                return LUA_PROFILE_DECLINE_TYPES;
            // (the fixed forms carry at most three: a vararg call is the plain FASTCALL)
            if (op != LOP_FASTCALL && i >= 2)
                break;
        }
    }
    if (shape.flags & B_TOSTRING)
    {
        const TValue* v = arg(0);
        if (!(ttisnil(v) || ttisboolean(v) || ttisnumber(v) || ttisstring(v) || ttisinteger(v)))
            return LUA_PROFILE_DECLINE_TYPES;
        if ((ttisnumber(v) || ttisinteger(v)) && luaC_needsGC(L))
            return LUA_PROFILE_DECLINE_GCDEBT;
        return LUA_PROFILE_DECLINE_VALUES;
    }
    if ((shape.flags & B_RAWLEN) && !ttistable(arg(0)) && !ttisstring(arg(0)))
        return LUA_PROFILE_DECLINE_TYPES;
    if ((shape.flags & B_SELECT) && !ttisnumber(arg(0)) && !(ttisstring(arg(0)) && *svalue(arg(0)) == '#'))
        return LUA_PROFILE_DECLINE_TYPES;
    if (shape.results == R_BYTE)
    {
        // (the bytes the range holds: as many results as that, or one for a vararg call)
        TString* ts = tsvalue(arg(0));
        int i = int(nvalue(arg(1)));
        int j = nparams >= 3 ? (ttisnumber(arg(2)) ? int(nvalue(arg(2))) : 0) : i;
        if (i >= 1 && j >= i && j <= int(ts->len) && (j - i + 1) != (nresults < 0 ? 1 : nresults))
            return LUA_PROFILE_DECLINE_RESULTS;
        return LUA_PROFILE_DECLINE_VALUES;
    }
    if ((shape.flags & B_GC) && !(shape.flags & B_GCFIRST) && luaC_needsGC(L))
        return LUA_PROFILE_DECLINE_GCDEBT;
    return LUA_PROFILE_DECLINE_VALUES;
}

// the outcome the last step left for this one to tell
static void resolve(lua_ProfileState* s, lua_State* L, const Instruction* pc)
{
    Pending pending = s->pending;
    s->pending.kind = PENDING_NONE;
    if (size_t(pending.function) >= s->functions.size())
        return;
    ProfFunction& f = s->functions[pending.function];
    switch (pending.kind)
    {
    case PENDING_FAST:
    {
        bool fell = L == pending.L && L->ci == pending.ci && pc == pending.next;
        if (f.siteof.empty() || f.siteof[pending.pc] >= 0)
            return;
        lua_ProfileBuiltinSite& site = f.builtinsites[-f.siteof[pending.pc] - 1];
        if (fell)
        {
            // (a declined call's fast call is its charge, and the fast path's dispatch)
            site.declines[pending.reason]++;
            site.charged += pending.price;
            site.fastequivalent += pending.price;
        }
        else
        {
            site.fast++;
        }
        return;
    }
    case PENDING_INSERT:
        outcome(f, pending.pc, pending.op, s->rehashes != pending.before ? LUA_PROFILE_REHASHED : LUA_PROFILE_INSERTED);
        return;
    case PENDING_DIRECT:
        outcome(f, pending.pc, pending.op, uint32_t(LUAU_INSN_AUX_SLOT(*pending.aux)) != pending.before ? LUA_PROFILE_REPREDICTED : LUA_PROFILE_HIT);
        return;
    }
}

// a CALL: a C function it reaches gets a node at this call site, and a frame (a Luau function it calls hangs under it; the
// next step in this frame ends it), and what it does until the next step is its own
static void callop(lua_ProfileState* s, lua_State* L, Closure* cl, const Instruction* pc, int at, int node, bool exempt)
{
    int reg = LUAU_INSN_A(*pc);
    int method = -1;
    for (size_t i = s->namecalls.size(); i-- > 0;)
    {
        const NameCall& n = s->namecalls[i];
        if (n.L == L && n.ci == L->ci && n.reg >= reg)
        {
            if (n.reg == reg && method < 0)
                method = n.name;
            s->namecalls.erase(s->namecalls.begin() + i);
        }
    }
    // (the engine's own Luau calls for its caller: its C calls are not listed apart)
    StkId ra = L->base + reg;
    if (exempt || !ttisfunction(ra) || !clvalue(ra)->isC)
        return;
    Closure* ccl = clvalue(ra);
    int cnode = child(s, node, -2 - cfunctionid(s, ccl->c.f, cnameof(s, ccl, method, cl->env, L->ci->p, at)), at);
    s->nodes[cnode].calls++;
    if (s->lastframes)
    {
        s->lastframes->push_back(ProfFrame{int(L->ci - L->base_ci) + 1, ccl, cnode, false, -1});
        s->cframe = true;
    }
    s->ctarget = cnode;
}

// a fresh call of the function at L->ci (C entered the interpreter for it): its frame, if the profile followed one there, is
// another call's, so the next step counts a new one (a comparator table.sort calls again and again at the same depth)
static void markfresh(lua_ProfileState* s, lua_State* L)
{
    CallInfo* ci = L->ci;
    if (!isLua(ci) || ci->savedpc != ci->p->code)
        return;
    std::vector<ProfFrame>* frames = NULL;
    if (L == s->lastL && s->lastframes)
    {
        frames = s->lastframes;
    }
    else
    {
        auto found = s->threads.find(L);
        if (found == s->threads.end())
            return;
        frames = &found->second;
    }
    int depth = int(ci - L->base_ci);
    if (!frames->empty() && frames->back().depth == depth)
        frames->back().returning = true;
}

void luaP_step(lua_State* L, Closure* cl, const Instruction* pc)
{
    lua_ProfileState* s = luaP_state;
    if (!s->stepping || s->g != L->global)
        return;
    Proto* p = L->ci->p;
    int op = LUAU_INSN_OP(*pc);

    if (s->pending.kind != PENDING_NONE)
        resolve(s, L, pc);

    book(s, L);
    s->ctarget = -1;
    s->owntable = NULL;

    // (the C function the last CALL reached returned without stepping elsewhere, or it called Luau, whose frames reconcile undoes)
    if (s->cframe)
    {
        s->cframe = false;
        if (L == s->lastL && L->ci == s->lastci && s->lastframes && !s->lastframes->empty())
        {
            const ProfFrame& top = s->lastframes->back();
            if (top.cl->isC && top.depth == int(L->ci - L->base_ci) + 1)
                s->lastframes->pop_back();
        }
    }

    int function = functionof(s, p);
    // the stack moved without a step seeing it, or the last instruction returned
    bool lastReturned = s->lastframes && !s->lastframes->empty() && s->lastframes->back().returning;
    if (L != s->lastL || L->ci != s->lastci || s->nodes[s->lastnode].function != function || lastReturned)
        reconcile(s, L);
    ProfFunction& f = s->functions[function];
    int node = s->lastnode;
    bool exempt = (p->flags & LUAU_PROTO_WORK_EXEMPT) != 0;
    // (the binding's helper chunks book like the host's own Luau, though their instructions count as a script's)
    bool booksbelow = exempt || s->functions[function].binding;
    int at = -1;

    if (op == LOP_NATIVECALL)
    {
        // entering the function's native code, which no step sees: the work booked until the next step is its
        f.nativeentries++;
        f.flags |= LUA_PROFILE_NATIVE;
        s->nativeseen = true;
    }
    else
    {
        at = int(pc - p->code);
        if (unsigned(at) >= unsigned(f.counts.size()))
            return;
        f.counts[at]++;
        f.phasecounts[phaselinesof(s, f)][at]++;
        s->nodes[node].insns++;
        if (!f.inlinesfound)
            findinlines(s, function);
        if (!f.inlineof.empty())
            enterinlines(s, f, at);

        switch (op)
        {
        case LOP_GETGLOBAL:
        case LOP_SETGLOBAL:
        case LOP_GETIMPORT:
        case LOP_GETTABLEKS:
        case LOP_SETTABLEKS:
        case LOP_NAMECALL:
        case LOP_GETUDATAKS:
        case LOP_SETUDATAKS:
        case LOP_NAMECALLUDATA:
            cacheop(s, L, cl, pc, op, f, function, at, node);
            break;
        case LOP_FASTCALL:
        case LOP_FASTCALL1:
        case LOP_FASTCALL2:
        case LOP_FASTCALL2K:
        case LOP_FASTCALL3:
        {
            lua_ProfileBuiltinSite& site = builtinsite(f, at, LUAU_INSN_A(*pc), s->phase, node);
            // (its own dispatch, at the host's price: a declined call's charge, and the fast path's)
            double price = s->pricing ? s->prices[(exempt ? kHostOpsSlot : (p->flags & LUAU_PROTO_WORK_NATIVE) ? kNativeOpsSlot : kOpsSlot) + op] : 0;
            if (!cl->env->safeenv)
            {
                // (an unsafe environment never takes the fast path)
                site.declines[LUA_PROFILE_DECLINE_UNSAFEENV]++;
                site.charged += price;
                site.fastequivalent += price;
            }
            else
            {
                s->pending = Pending();
                s->pending.kind = PENDING_FAST;
                s->pending.L = L;
                s->pending.ci = L->ci;
                s->pending.next = pc + ((op == LOP_FASTCALL || op == LOP_FASTCALL1) ? 1 : 2);
                s->pending.function = function;
                s->pending.pc = at;
                s->pending.reason = declinereason(L, cl, pc, op);
                s->pending.price = price;
            }
            break;
        }
        case LOP_CALL:
        case LOP_CALLFB:
            callop(s, L, cl, pc, at, node, exempt);
            break;
        case LOP_RETURN:
            if (s->lastframes && !s->lastframes->empty())
            {
                s->lastframes->back().returning = true;
                // (back into the C function that called it: what follows until the next step is that function's)
                size_t n = s->lastframes->size();
                if (!exempt && (L->ci->flags & LUA_CALLINFO_RETURN) && n >= 2)
                {
                    const ProfFrame& below = (*s->lastframes)[n - 2];
                    if (below.cl->isC && below.depth == s->lastframes->back().depth - 1)
                        s->ctarget = below.node;
                }
            }
            break;
        }
    }

    if (booksbelow)
    {
        s->target = s->hosttarget;
        s->targetpc = s->hosttargetpc;
        s->targetnode = s->hosttargetnode;
    }
    else
    {
        s->target = function;
        s->targetpc = at;
        s->targetnode = node;
    }
    s->lastop = op;
    s->lastbase = exempt ? kHostOpsSlot : (p->flags & LUAU_PROTO_WORK_NATIVE) ? kNativeOpsSlot : kOpsSlot;
    bookcallee(s);

    // an object a cold charge was paid for in this frame, touched by another function: a cut of the payer leaves the charge here
    if (!s->coldframe.empty() && at >= 0)
    {
        GCObject* touched = NULL;
        StkId base = L->base;
        Instruction insn = *pc;
        switch (op)
        {
        case LOP_GETTABLEKS:
        case LOP_SETTABLEKS:
        case LOP_GETTABLE:
        case LOP_SETTABLE:
        case LOP_GETTABLEN:
        case LOP_SETTABLEN:
        case LOP_NAMECALL:
        case LOP_LENGTH:
        {
            StkId rb = base + LUAU_INSN_B(insn);
            if (ttistable(rb))
                touched = gcvalue(rb);
            break;
        }
        case LOP_FORGLOOP:
        {
            StkId ra = base + LUAU_INSN_A(insn);
            if (ttistable(ra + 1))
                touched = gcvalue(ra + 1);
            break;
        }
        case LOP_CALL:
        case LOP_CALLFB:
        {
            StkId ra = base + LUAU_INSN_A(insn);
            if (ttisfunction(ra))
                touched = gcvalue(ra);
            break;
        }
        }
        if (touched)
        {
            auto found = s->coldframe.find(touched);
            if (found != s->coldframe.end())
            {
                int payer = found->second.kind == LUA_COLD_CFUNCTION && s->ctarget >= 0 ? s->nodes[s->ctarget].function : s->target;
                if (found->second.payer != payer)
                    found->second.shared = true;
            }
        }
    }
}

void luaP_nativeenter(lua_State* L, Closure* cl)
{
    lua_ProfileState* s = luaP_state;
    if (!s || !s->stepping || s->g != L->global || cl->isC)
        return;
    book(s, L);
    s->ctarget = -1;
    s->owntable = NULL;
    int function = functionof(s, L->ci->p);
    if (L != s->lastL || L->ci != s->lastci)
        reconcile(s, L);
    ProfFunction& f = s->functions[function];
    f.nativeentries++;
    f.flags |= LUA_PROFILE_NATIVE;
    s->nativeseen = true;
    // (the host's own Luau and the binding's helper chunks, natively compiled too, book to the script line that called them)
    if ((L->ci->p->flags & LUAU_PROTO_WORK_EXEMPT) || f.binding)
    {
        s->target = s->hosttarget;
        s->targetpc = s->hosttargetpc;
        s->targetnode = s->hosttargetnode;
    }
    else
    {
        s->target = function;
        s->targetpc = -1;
        s->targetnode = s->lastnode;
    }
    // (native code dispatches no instruction: none is priced at the next booking)
    s->lastop = -1;
    bookcallee(s);
}

// The C stack grows down on every target ree builds for: a frame called later sits at a lower address
static bool deeper(uintptr_t sp, uintptr_t than)
{
    return sp < than;
}

// the entries from the innermost to (and with) the first one not deeper than `sp` are gone, `through` that one when set, else
// stopping above it: the C function the outermost gone one came from is back in charge (it returned, an error unwound it, its
// thread yielded), or the engine is when none is left
static void left(lua_ProfileState* s, lua_State* L, uintptr_t sp, bool through)
{
    int from = -1;
    bool any = false;
    while (!s->entries.empty() && (deeper(s->entries.back().sp, sp) || (through && s->entries.back().sp == sp)))
    {
        from = s->entries.back().from;
        s->entries.pop_back();
        any = true;
    }
    if (!any)
        return;
    if (!s->entries.empty())
    {
        if (s->stepping)
        {
            // what was charged so far is the last instruction's and the C function it reached (an error's); what is charged until
            // the next step is the C function's the entry came from
            book(s, L);
            s->ctarget = from;
        }
        return;
    }
    if (s->stepping)
    {
        // the last instruction's charges are its own (and a C function's it reached: one that yielded); what is charged until the
        // next one runs is the engine's
        book(s, L);
        s->target = -1;
        s->ctarget = -1;
    }
    if (s->vm)
        s->vm(s->vmud, L, 0);
}

void luaP_enter(lua_State* L, const void* frame)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    uintptr_t sp = uintptr_t(frame);
    // (entries an error unwound with no catch that told the profile: deeper than this one)
    left(s, L, sp, false);
    // (the C function this entry came from: the frame the CALL that reached it pushed, innermost in the frames stepped last)
    int from = -1;
    if (s->stepping && !s->entries.empty() && s->lastframes && !s->lastframes->empty() && s->lastframes->back().cl->isC)
        from = s->lastframes->back().node;
    s->entries.push_back(ProfEntry{sp, from});
    if (s->entries.size() == 1 && s->vm)
        s->vm(s->vmud, L, 1);
    if (s->stepping)
        markfresh(s, L);
}

void luaP_leave(lua_State* L, const void* frame)
{
    lua_ProfileState* s = stateof(L);
    if (s)
        left(s, L, uintptr_t(frame), true);
}

void luaP_unwound(lua_State* L, const void* frame)
{
    lua_ProfileState* s = stateof(L);
    if (s)
        left(s, L, uintptr_t(frame), false);
}

void luaP_loaded(lua_State* L, Proto* main)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->loadchunks)
        return;
    // every function of the chunk (the main function defines the rest), and the copies its compiler inlined, found now that
    // the chunk's functions are all known: a function that only ever runs as copies is named, and a sample on its lines is its
    int id = functionof(s, main);
    std::vector<int> functions = s->chunks[s->functions[id].chunk].functions;
    for (int function : functions)
        if (!s->functions[function].inlinesfound)
            findinlines(s, function);
}

void luaP_threadgone(lua_State* L, lua_State* L1)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    if (s->lastL == L1)
    {
        s->lastL = NULL;
        s->lastci = NULL;
        s->lastframes = NULL;
        s->cframe = false;
    }
    if (s->pending.L == L1)
        s->pending.kind = PENDING_NONE;
    for (size_t i = s->namecalls.size(); i-- > 0;)
        if (s->namecalls[i].L == L1)
            s->namecalls.erase(s->namecalls.begin() + i);
    s->threads.erase(L1);
    s->resumes.erase(L1);
    // (a freed thread's sampled calls are gone without a word: their functions' counts give them up; a reset one's left
    // first, lua_resetthread)
    auto open = s->callopen.find(L1);
    if (open != s->callopen.end())
    {
        for (int id : open->second)
            if (size_t(id) < s->callsamples.size() && s->callsamples[id].open > 0)
                s->callsamples[id].open--;
        s->callopen.erase(open);
    }
}

void luaP_resumed(lua_State* L)
{
    lua_ProfileState* s = stateof(L);
    if (s && s->stepping)
        s->resumes[L]++;
}

unsigned long long lua_profileresumes(lua_State* L)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping)
        return 0;
    auto found = s->resumes.find(L);
    return found == s->resumes.end() ? 0 : found->second;
}

void luaP_freed(lua_State* L, GCObject* o)
{
    lua_ProfileState* s = stateof(L);
    if (s && s->freed)
        s->freed(s->freedud, o);
}

void luaP_coldcode(lua_State* L, Proto* p, int pc)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping || !s->pricing || s->coldcodeslot < 0)
        return;
    double price = s->prices[s->coldcodeslot];
    int function = functionof(s, p);
    if (L != s->lastL || L->ci != s->lastci || s->nodes[s->lastnode].function != function)
        reconcile(s, L);
    // luaP_step has reconciled this executing frame. A binding helper's normal work
    // can target its caller, but the helper's code still belongs to this frame.
    int node = s->lastnode;
    LUAU_ASSERT(s->nodes[node].function == function);
    ProfFunction& f = s->functions[function];
    LUAU_ASSERT(unsigned(pc) < f.work.size());
    f.work[pc] += price;
    f.phasework[phaselinesof(s, f)][pc] += price;
    s->nodes[node].work += price;
    double kinds[LUA_PROFILE_SPLITS] = {};
    kinds[LUA_PROFILE_COLD] = price;
    addkinds(s, function, pc, node, kinds);
    addexclusive(s, function, pc, node, price);
    if (lua_ProfileBuiltinSite* site = fallbacksite(f, pc, node, s->phase))
        site->charged += price;
}

void luaP_cold(lua_State* L, GCObject* o, int kind, int slot)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping || !s->pricing || s->target < 0 || slot < 0 || size_t(slot) >= s->prices.size())
        return;
    ColdRecord record = {kind, s->target, s->targetpc, s->targetnode, s->prices[slot], false, -1, -1};
    if (kind == LUA_COLD_TABLE)
    {
        auto site = s->tablesites.find(o);
        if (site != s->tablesites.end())
        {
            record.sitefunction = int(int64_t(site->second) >> 32);
            record.sitepc = int(uint32_t(site->second));
        }
    }
    else if (kind == LUA_COLD_CFUNCTION)
    {
        record.sitefunction = cfunctionof(s, gco2cl(o), L->ci, L->base_ci);
        if (s->ctarget >= 0)
        {
            record.payer = s->nodes[s->ctarget].function;
            record.pc = -1;
            record.node = s->ctarget;
        }
    }
    else
        record.sitefunction = functionof(s, gco2cl(o)->l.p);
    s->coldframe[o] = record;
}

// settles the frame's cold charges for objects: their totals, and the payers' charges nothing else shared
static void coldsettle(lua_ProfileState* s)
{
    for (auto& entry : s->coldframe)
    {
        const ColdRecord& r = entry.second;
        uint64_t key = (uint64_t(uint32_t(r.kind)) << 60) ^ (uint64_t(uint32_t(r.sitefunction)) << 28) ^ uint64_t(uint32_t(r.sitepc) & 0x0fffffff);
        ColdTotal& total = s->coldtotals[key];
        total.kind = r.kind;
        total.function = r.sitefunction;
        total.pc = r.sitepc;
        total.cost += r.cost;
        total.frames++;
        total.sharedframes += r.shared ? 1 : 0;
        total.payers[(uint64_t(uint32_t(r.payer)) << 32) | uint32_t(r.pc)] += r.cost;
        if (!r.shared && r.node >= 0 && size_t(r.node) < s->nodes.size())
            addexclusive(s, r.payer, r.pc, r.node, r.cost);
    }
    s->coldframe.clear();
}

void luaP_coldframe(lua_State* L)
{
    lua_ProfileState* s = stateof(L);
    if (s && s->stepping)
        coldsettle(s);
}

void luaP_tablemade(lua_State* L, LuaTable* t)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping)
        return;
    s->tablesites[t] = (uint64_t(uint32_t(s->target)) << 32) | uint32_t(s->target >= 0 ? s->targetpc : -1);
}

void lua_profilesplit(lua_State* L, const signed char* kinds, int nkinds, const double* hostkinds)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->pricing || nkinds != kWorkSlots)
        return;
    s->splitting = true;
    s->kinds.assign(kinds, kinds + nkinds);
    s->hostkinds = hostkinds;
    if (hostkinds)
        for (int k = 0; k < LUA_PROFILE_SPLITS; ++k)
            s->lasthostkinds[k] = hostkinds[k];
    // the callee's slots and the cold code's leave the per-step booking
    s->calleeslots = {int(offsetof(lua_WorkCounters, coldclosures) / sizeof(uint64_t)), int(offsetof(lua_WorkCounters, coldprotos) / sizeof(uint64_t)),
        kHostOpsSlot + LUA_WORK_RAREPROTOS};
    s->coldcodeslot = int(offsetof(lua_WorkCounters, coldcode) / sizeof(uint64_t));
    std::vector<int> scalars;
    for (int slot : s->scalars)
        if (slot != s->coldcodeslot && std::find(s->calleeslots.begin(), s->calleeslots.end(), slot) == s->calleeslots.end())
            scalars.push_back(slot);
    s->scalars = scalars;
}

int lua_profilecoldobjects(lua_State* L, void (*each)(void* ud, const lua_ProfileColdObject* o), void* ud)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return 0;
    coldsettle(s);
    int count = 0;
    for (auto& entry : s->coldtotals)
    {
        ColdTotal& total = entry.second;
        total.listed.clear();
        for (auto& payer : total.payers)
            total.listed.push_back(lua_ProfileColdPayer{int(int64_t(payer.first) >> 32), int(uint32_t(payer.first)), payer.second});
        // the dearest payer first; payers that cost the same by function and then pc, so the order does not depend on the map's
        std::sort(total.listed.begin(), total.listed.end(), [](const lua_ProfileColdPayer& a, const lua_ProfileColdPayer& b) {
            if (a.cost != b.cost)
                return a.cost > b.cost;
            if (a.function != b.function)
                return a.function < b.function;
            return a.pc < b.pc;
        });
        lua_ProfileColdObject out = {total.kind, total.function, total.pc, total.cost, total.frames, total.sharedframes, int(total.listed.size()),
            total.listed.empty() ? NULL : total.listed.data()};
        each(ud, &out);
        count++;
    }
    return count;
}

void lua_profilebooknext(lua_State* L, lua_State* co)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping || !co)
        return;
    // (what was charged so far is the last instruction's or the engine's)
    book(s, L);
    for (CallInfo* ci = co->ci; ci > co->base_ci; --ci)
    {
        if (!isLua(ci))
            continue;
        reconcile(s, co);
        Proto* p = ci->p;
        if ((p->flags & LUAU_PROTO_WORK_EXEMPT) || s->functions[functionof(s, p)].binding)
        {
            s->target = s->hosttarget;
            s->targetpc = s->hosttargetpc;
            s->targetnode = s->hosttargetnode;
            return;
        }
        int function = functionof(s, p);
        int depth = int(ci - co->base_ci);
        int node = s->lastnode;
        for (const ProfFrame& frame : *s->lastframes)
            if (frame.depth == depth)
                node = frame.node;
        s->target = function;
        s->targetpc = (ci->flags & LUA_CALLINFO_NATIVE) ? -1 : pcRel(ci->savedpc, p);
        s->targetnode = node;
        return;
    }
    // not started: the function on top of its stack runs as the thread's outermost frame
    if (co->top > co->stack && ttisfunction(co->top - 1) && !clvalue(co->top - 1)->isC)
    {
        Proto* p = clvalue(co->top - 1)->l.p;
        if (p->flags & LUAU_PROTO_WORK_EXEMPT)
            return;
        int function = functionof(s, p);
        s->target = function;
        s->targetpc = 0;
        s->targetnode = child(s, s->phaseroot, function, -1);
    }
}

int lua_profilecurrent(int* function, int* pc, int* node, unsigned long long* calls)
{
    lua_ProfileState* s = luaP_state;
    if (!s || !s->stepping)
        return 0;
    if (function)
        *function = s->target;
    if (pc)
        *pc = s->targetpc;
    if (node)
        *node = s->targetnode;
    if (calls)
        *calls = s->targetnode >= 0 && size_t(s->targetnode) < s->nodes.size() ? s->nodes[s->targetnode].calls : 0;
    return 1;
}

int lua_profilestart(lua_State* L, int stepping, const double* prices, int nprices, const double* hostwork, const double* hostdrained)
{
    if (luaP_state)
        return 0;
    lua_ProfileState* s = new lua_ProfileState();
    s->g = L->global;
    s->stepping = stepping != 0;
    if (s->stepping && prices && nprices == kWorkSlots)
    {
        s->pricing = true;
        s->prices.assign(prices, prices + nprices);
        for (int slot = 0; slot < kWorkSlots; ++slot)
        {
            // (an opcode range's slots past the last opcode are counters of their own: hostops' cold C functions, cold host
            // functions and rare functions, LUA_WORK_COLDCFUNCS and the rest; booked when they move, as the counters past the ranges)
            bool ops = (slot >= kOpsSlot && slot < kOpsSlot + LOP__COUNT) || (slot >= kHostOpsSlot && slot < kHostOpsSlot + LOP__COUNT) ||
                       (slot >= kNativeOpsSlot && slot < kNativeOpsSlot + LOP__COUNT);
            bool unused = (slot >= kOpsSlot + LOP__COUNT && slot < kOpsSlot + 256) || (slot >= kNativeOpsSlot + LOP__COUNT && slot < kNativeOpsSlot + 256) ||
                          (slot >= kHostOpsSlot + LOP__COUNT && slot < kHostOpsSlot + 256 && prices[slot] == 0);
            if (!ops && !unused)
                s->scalars.push_back(slot);
        }
        s->hostwork = hostwork;
        s->hostdrained = hostdrained;
        s->lasthost = hostwork && hostdrained ? *hostwork + *hostdrained : 0;
    }
    s->last = L->global->work;
    ProfNode root = {-1, -1, -1, 0, 0, 0.0, {}, 0.0};
    s->nodes.push_back(root);
    luaP_allockind = 0;
    luaP_allocblock = NULL;
    luaP_state = s;
    return 1;
}

void lua_profilestop(lua_State* L)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping)
        return;
    // (an outcome the next step would have told is not known)
    s->pending.kind = PENDING_NONE;
    book(s, L);
    bookcallee(s);
    coldsettle(s);
    s->stepping = false;
}

int lua_profilefunctions(lua_State* L, void (*each)(void* ud, int id, const lua_ProfileFunction* f), void* ud)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return 0;
    for (size_t i = 0; i < s->functions.size(); ++i)
    {
        const ProfFunction& f = s->functions[i];
        lua_ProfileFunction out;
        out.source = f.source.c_str();
        out.name = f.name.c_str();
        out.linedefined = f.linedefined;
        out.flags = f.flags;
        out.sizecode = int(f.code.size());
        out.code = f.code.data();
        out.lines = f.lines.data();
        out.counts = f.counts.empty() ? NULL : f.counts.data();
        out.work = f.work.empty() ? NULL : f.work.data();
        out.ncaches = int(f.cachesites.size());
        out.caches = f.cachesites.empty() ? NULL : f.cachesites.data();
        out.nbuiltins = int(f.builtinsites.size());
        out.builtins = f.builtinsites.empty() ? NULL : f.builtinsites.data();
        out.nativeentries = f.nativeentries;
        out.nativework = f.nativework;
        out.ninlines = int(f.inlines.size());
        out.inlines = f.inlines.empty() ? NULL : f.inlines.data();
        out.split = f.split.empty() ? NULL : f.split.data();
        out.exclusive = f.exclusive.empty() ? NULL : f.exclusive.data();
        ProfFunction& lined = s->functions[i];
        for (size_t k = 0; k < lined.phaselines.size(); ++k)
        {
            lined.phaselines[k].counts = lined.phasecounts[k].data();
            lined.phaselines[k].work = lined.phasework[k].data();
            lined.phaselines[k].cold = lined.phasecold[k].data();
        }
        out.nphaselines = int(lined.phaselines.size());
        out.phaselines = lined.phaselines.empty() ? NULL : lined.phaselines.data();
        out.binding = f.binding ? 1 : 0;
        each(ud, int(i), &out);
    }
    return int(s->functions.size());
}

int lua_profilenodes(lua_State* L, void (*each)(void* ud, int id, const lua_ProfileNode* n), void* ud)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return 0;
    for (size_t i = 0; i < s->nodes.size(); ++i)
    {
        const ProfNode& n = s->nodes[i];
        lua_ProfileNode out = {n.parent, n.function, n.callpc, n.calls, n.insns, n.work, {}, n.exclusive};
        for (int k = 0; k < LUA_PROFILE_SPLITS; ++k)
            out.split[k] = n.split[k];
        each(ud, int(i), &out);
    }
    return int(s->nodes.size());
}

const char* lua_profilecfunction(lua_State* L, int index)
{
    lua_ProfileState* s = stateof(L);
    if (!s || index < 0 || size_t(index) >= s->cfunctions.size())
        return NULL;
    return s->names[s->cfunctions[index].name].c_str();
}

const char* lua_profilename(lua_State* L, int id)
{
    lua_ProfileState* s = stateof(L);
    if (!s || id < 0 || size_t(id) >= s->names.size())
        return NULL;
    return s->names[id].c_str();
}

const char* lua_profilebuiltinname(int id)
{
    return id > 0 && id < kBuiltinCount ? kBuiltins[id].name : NULL;
}

void lua_profilephase(lua_State* L, int phase)
{
    lua_ProfileState* s = stateof(L);
    if (!s || !s->stepping)
        return;
    // (what was charged so far is the instruction's that made it; the frames are followed again under the new phase)
    book(s, L);
    s->phaseroot = phase <= 0 ? 0 : child(s, 0, -1, phase);
    s->phase = phase < 0 ? 0 : phase;
    s->threads.clear();
    s->lastL = NULL;
    s->lastci = NULL;
    s->lastframes = NULL;
    s->cframe = false;
}

double lua_profileengine(lua_State* L)
{
    lua_ProfileState* s = stateof(L);
    return s ? s->enginework : 0;
}

void lua_profilefree(lua_State* L)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    luaP_state = NULL;
    s->g->profilecalls = 0;
    delete s;
}

void lua_setprofilecalls(lua_State* L, void (*hook)(void* ud, lua_State* L, int event, int function, int every, int depth), void* ud)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    s->callhook = hook;
    s->callud = ud;
    s->g->profilecalls = hook != NULL;
}

void lua_setprofilecallevery(lua_State* L, int function, int every, int remeasure)
{
    lua_ProfileState* s = stateof(L);
    if (!s || function < 0)
        return;
    if (size_t(function) >= s->callsamples.size())
        s->callsamples.resize(size_t(function) + 1);
    CallSample& sample = s->callsamples[function];
    sample.every = every < 0 ? 0 : every;
    sample.remeasure = remeasure < 0 ? 0 : remeasure;
    sample.left = sample.remeasure;
}

void luaP_callenter(lua_State* L)
{
    CallInfo* ci = L->ci;
    Proto* p = ci->p;
    lua_ProfileState* s = stateof(L);
    // (native code returns without the interpreter's RETURN: its calls are not sampled; a call's frame has the native flag
    // only once its NATIVECALL runs, so the function is asked)
    if (!s || !s->callhook || (ci->flags & LUA_CALLINFO_NATIVE) || (p->exectarget != 0 && p->execdata))
        return;
    int id = functionof(s, p);
    if (size_t(id) >= s->callsamples.size())
        s->callsamples.resize(size_t(id) + 1);
    CallSample& sample = s->callsamples[id];
    int every = sample.every;
    if (every > 1 && sample.open > 0 && sample.nestedleft > 0 && insampledcall(L, ci, p))
    {
        // inside a sampled call of its own function on this thread: sampled with it (a chance of 1, which the hook is
        // told), so a recursion's estimate is as steady as its outermost call's instead of noisier with every level
        sample.nestedleft--;
        every = 1;
    }
    else if (every <= 0)
    {
        // none sampled: one call after every `remeasure` read around for its length alone
        if (sample.remeasure <= 0 || --sample.left > 0)
            return;
        sample.left = sample.remeasure;
        every = 0;
    }
    else if (every > 1)
    {
        // each call drawn on its own (xorshift64): a call nested in another of its function is as likely as any
        uint64_t x = s->callrandom;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s->callrandom = x;
        if ((x >> 11) % uint64_t(every) != 0)
            return;
        // (the outermost sampled call of its function on this thread: the calls of it inside may be read with it; a
        // call on another thread, or one a dead thread left open, does not hold the budget back)
        if (sample.open == 0 || !insampledcall(L, ci, p))
            sample.nestedleft = kNestedReads;
    }
    sample.open++;
    s->callopen[L].push_back(id);
    ci->flags |= LUA_CALLINFO_PROFILECALL;
    // (the frame's first instruction: the hook may read the stack, this frame's line included)
    ci->savedpc = p->code;
    s->callhook(s->callud, L, LUA_PROFILECALL_ENTER, id, every, int(ci - L->base_ci));
}

void luaP_callleave(lua_State* L, CallInfo* ci)
{
    ci->flags &= ~LUA_CALLINFO_PROFILECALL;
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    int id = functionof(s, ci->p);
    // (leaves come innermost first: the thread's last open call, unless the hook was set while calls were open)
    auto open = s->callopen.find(L);
    if (open != s->callopen.end())
    {
        std::vector<int>& calls = open->second;
        for (size_t i = calls.size(); i-- > 0;)
            if (calls[i] == id)
            {
                calls.erase(calls.begin() + i);
                if (size_t(id) < s->callsamples.size() && s->callsamples[id].open > 0)
                    s->callsamples[id].open--;
                break;
            }
        if (calls.empty())
            s->callopen.erase(open);
    }
    if (s->callhook)
        s->callhook(s->callud, L, LUA_PROFILECALL_LEAVE, id, 0, int(ci - L->base_ci));
}

void luaP_callsunwound(lua_State* L, CallInfo* to)
{
    for (CallInfo* ci = L->ci; ci > to; --ci)
        if (ci->flags & LUA_CALLINFO_PROFILECALL)
            luaP_callleave(L, ci);
}

int lua_profilestack(lua_State* L, int* functions, int* lines, int max)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return 0;
    int n = 0;
    for (CallInfo* ci = L->ci; ci > L->base_ci && n < max; --ci)
    {
        if (!ttisfunction(ci->func))
            continue;
        Closure* cl = clvalue(ci->func);
        if (cl->isC)
        {
            functions[n] = -2 - cfunctionof(s, cl, ci, L->base_ci);
            lines[n] = -1;
        }
        else
        {
            Proto* p = ci->p;
            functions[n] = functionof(s, p);
            lines[n] = (ci->flags & LUA_CALLINFO_NATIVE) ? -1 : luaG_getline(p, pcRel(ci->savedpc, p));
        }
        n++;
    }
    return n;
}

int lua_profilepc(lua_State* L, int* function)
{
    *function = -1;
    lua_ProfileState* s = stateof(L);
    if (!s)
        return -1;
    for (CallInfo* ci = L->ci; ci > L->base_ci; --ci)
    {
        if (!ttisfunction(ci->func) || clvalue(ci->func)->isC)
            continue;
        if (ci->flags & LUA_CALLINFO_NATIVE)
            return -1;
        Proto* p = ci->p;
        *function = functionof(s, p);
        return pcRel(ci->savedpc, p);
    }
    return -1;
}

int lua_profileentries(lua_State* L, int* incfunction)
{
    *incfunction = L->ci > L->base_ci && !isLua(L->ci);
    lua_ProfileState* s = stateof(L);
    return s ? int(s->entries.size()) : 0;
}

void lua_profiletoolchunks(lua_State* L, const char* const* prefixes, int n)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    s->toolprefixes.clear();
    for (int i = 0; i < n; ++i)
        s->toolprefixes.push_back(prefixes[i]);
    for (ProfFunction& f : s->functions)
        f.tool = -1;
}

void lua_profilechunks(lua_State* L, int on)
{
    if (lua_ProfileState* s = stateof(L))
    {
        s->loadchunks = on != 0;
    }
}


int lua_profileop(lua_State* L)
{
    CallInfo* ci = L->ci;
    if (!isLua(ci) || (ci->flags & LUA_CALLINFO_NATIVE) || !ci->savedpc)
        return -1;
    Proto* p = ci->p;
    if (ci->savedpc <= p->code || ci->savedpc > p->code + p->sizecode)
        return -1;
    return LUAU_INSN_OP(ci->savedpc[-1]);
}

void lua_setprofilevm(lua_State* L, void (*vm)(void* ud, lua_State* L, int entered), void* ud)
{
    if (lua_ProfileState* s = stateof(L))
    {
        s->vm = vm;
        s->vmud = ud;
    }
}

void* lua_allocblock(lua_State* L, int* kind)
{
    (void)L;
    if (kind)
        *kind = luaP_allocblock ? LUA_ALLOC_OBJECT : luaP_allockind;
    return luaP_allocblock;
}

int lua_profileobject(void* block, int* tag, void** data, const char** type)
{
    GCObject* o = (GCObject*)block;
    if (tag)
        *tag = -1;
    if (data)
        *data = NULL;
    if (type)
        *type = NULL;
    if (o->gch.tt != LUA_TUSERDATA)
        return o->gch.tt;
    Udata* u = gco2u(o);
    if (tag)
        *tag = u->tag;
    if (data)
        *data = u->data;
    if (type && u->metatable && u->metatable->node != &luaH_dummynode)
    {
        LuaTable* h = u->metatable;
        for (int i = 0; i < sizenode(h); ++i)
        {
            const LuaNode& n = h->node[i];
            if (ttisstring(&n.key) && ttisstring(&n.val) && strcmp(svalue(&n.key), "__type") == 0)
            {
                *type = svalue(&n.val);
                break;
            }
        }
    }
    return LUA_TUSERDATA;
}

size_t lua_profileudataoffset(void)
{
    return offsetof(Udata, data);
}

void lua_profilemark(void* block)
{
    GCObject* o = (GCObject*)block;
    o->gch.marked |= bitmask(PROFBIT);
}

void lua_setprofilefreed(lua_State* L, void (*freed)(void* ud, void* block), void* ud)
{
    if (lua_ProfileState* s = stateof(L))
    {
        s->freed = freed;
        s->freedud = ud;
    }
}

void lua_setprofilerehash(
    lua_State* L,
    void (*rehash)(void* ud, lua_State* L, void* table, int oldarray, int oldnodes, int newarray, int newnodes, int keys, int script),
    void* ud
)
{
    if (lua_ProfileState* s = stateof(L))
    {
        s->rehash = rehash;
        s->rehashud = ud;
    }
}

void luaP_rehash(lua_State* L, LuaTable* t, int oldarray, int oldnodes, int keys)
{
    lua_ProfileState* s = stateof(L);
    if (!s)
        return;
    s->rehashes++;
    // (an insert at a cache site that rehashed, as the host charges it (a script's own instruction): allocating the table's storage at
    // this size up front would have charged the bytes it adds over the storage the table had, at the host's price for storage)
    if (s->pricing && s->pending.kind == PENDING_INSERT && s->pending.L == L && s->pending.ci == L->ci && luaE_scriptinsn(L))
    {
        ProfFunction& f = s->functions[s->pending.function];
        int slot = size_t(s->pending.pc) < f.siteof.size() ? f.siteof[s->pending.pc] : 0;
        if (slot > 0)
        {
            int nodes = t->node == &luaH_dummynode ? 0 : sizenode(t);
            double bytes = double(std::max(0, t->sizearray - oldarray)) * sizeof(TValue) + double(std::max(0, nodes - oldnodes)) * sizeof(LuaNode);
            // Presizing pays for the final storage once, including allocations outside
            // the VM's pages. Repeated growth pays for each intermediate allocation.
            int blocks = (t->sizearray * sizeof(TValue) > 1024) - (oldarray * sizeof(TValue) > 1024) +
                         (nodes * sizeof(LuaNode) > 1024) - (oldnodes * sizeof(LuaNode) > 1024);
            f.cachesites[slot - 1].presized += bytes * (s->prices[kStoredSlot] + s->prices[kHostOpsSlot + LUA_WORK_ALLOCBYTES]) +
                                                   blocks * s->prices[kHostOpsSlot + LUA_WORK_LARGEBLOCKS];
        }
    }
    if (!s->rehash)
        return;
    int nodes = t->node == &luaH_dummynode ? 0 : sizenode(t);
    s->rehash(s->rehashud, L, t, oldarray, oldnodes, t->sizearray, nodes, keys, luaE_scriptinsn(L) ? 1 : 0);
}

void luaP_callstorage(lua_State* L, size_t bytes)
{
    lua_ProfileState* s = stateof(L);
    if (s && s->stepping && s->pricing && luaE_scriptwork(L) && !luaE_hostcall(L))
        s->callstored += bytes;
}

void luaP_coldtouch(lua_State* L, GCObject* o)
{
    lua_ProfileState* s = luaP_state;
    if (!s || !s->stepping || !s->pricing || s->g != L->global || !s->owntable || obj2gco(s->owntable) != o)
        return;
    s->owntouch += s->prices[kColdTablesSlot];
}

int lua_profiletable(void* ptr, int* arrayslots, int* arrayused, int* nodes, int* nodesused)
{
    GCObject* o = (GCObject*)ptr;
    if (o->gch.tt != LUA_TTABLE)
        return 0;
    LuaTable* h = gco2h(o);
    int used = 0;
    for (int i = 0; i < h->sizearray; ++i)
        if (!ttisnil(&h->array[i]))
            used++;
    int size = h->node == &luaH_dummynode ? 0 : sizenode(h);
    int nodeused = 0;
    for (int i = 0; i < size; ++i)
        if (!ttisnil(gval(&h->node[i])))
            nodeused++;
    if (arrayslots)
        *arrayslots = h->sizearray;
    if (arrayused)
        *arrayused = used;
    if (nodes)
        *nodes = size;
    if (nodesused)
        *nodesused = nodeused;
    return 1;
}

void lua_profiletablebytes(size_t* header, size_t* slot, size_t* node)
{
    if (header)
        *header = sizeof(LuaTable);
    if (slot)
        *slot = sizeof(TValue);
    if (node)
        *node = sizeof(LuaNode);
}

// the cache kind of an instruction with an inline cache (lua_profilecachetotals' order), -1 for the others
static int cachekind(int op)
{
    switch (op)
    {
    case LOP_GETGLOBAL:
    case LOP_SETGLOBAL:
        return 0;
    case LOP_GETIMPORT:
        return 1;
    case LOP_NAMECALL:
    case LOP_NAMECALLUDATA:
        return 2;
    case LOP_GETTABLEKS:
    case LOP_SETTABLEKS:
    case LOP_GETUDATAKS:
    case LOP_SETUDATAKS:
        return 3;
    }
    return -1;
}

int lua_profilecachetotals(lua_State* L, uint64_t* out)
{
    const int depths = 4 * LUA_PROFILE_CACHES + 1 + LUA_PROFILE_DECLINES;
    const int total = depths + 4 * LUA_PROFILE_DEPTHS;
    for (int i = 0; i < total; ++i)
        out[i] = 0;
    lua_ProfileState* s = stateof(L);
    if (!s)
        return 0;
    for (ProfFunction& f : s->functions)
    {
        if (f.flags & LUA_PROFILE_EXEMPT)
            continue;
        if (f.tool < 0)
        {
            f.tool = 0;
            for (const std::string& prefix : s->toolprefixes)
                if (f.source.compare(0, prefix.size(), prefix) == 0)
                    f.tool = 1;
        }
        if (f.tool)
            continue;
        for (const lua_ProfileCacheSite& site : f.cachesites)
        {
            int kind = cachekind(site.op);
            if (kind < 0)
                continue;
            for (int outcome = 0; outcome < LUA_PROFILE_CACHES; ++outcome)
                out[kind * LUA_PROFILE_CACHES + outcome] += site.outcomes[outcome];
            for (int depth = 0; depth < LUA_PROFILE_DEPTHS; ++depth)
                out[depths + kind * LUA_PROFILE_DEPTHS + depth] += site.depths[depth];
        }
        for (const lua_ProfileBuiltinSite& site : f.builtinsites)
        {
            out[4 * LUA_PROFILE_CACHES] += site.fast;
            for (int reason = 0; reason < LUA_PROFILE_DECLINES; ++reason)
                out[4 * LUA_PROFILE_CACHES + 1 + reason] += site.declines[reason];
        }
    }
    return 1;
}

// The heap as the collector sees it: luaC_enumheap's nodes (lgcdebug.cpp: the same sizes and names) and every reference
// the collector follows (lgc.cpp's traverse functions and cleartable), which differs from luaC_enumheap's edges in a
// prototype's source, debug name, upvalue and local names, a thread's namecall and open upvalues, the strings of a weak
// table (never cleared) and a weak-valued array part (not followed); and its roots (markroot, atomic) as edges from NULL:
// the main thread, the registry, the basic types' metatables and the fixed objects. So an object no edge reaches is one
// the next collection frees. Objects the collector found dead and has not swept yet go to `dead`, by type and size only:
// during a sweep a dead closure can hold a prototype already freed (luaC_enumheap asserts on it).
namespace
{
struct HeapWalk
{
    lua_State* L;
    void* context;
    void (*node)(void* context, void* ptr, uint8_t tt, uint8_t memcat, size_t size, const char* name);
    void (*edge)(void* context, void* from, void* to, const char* name);
    void (*dead)(void* context, uint8_t tt, size_t size);
};

void* walkpointer(GCObject* gco)
{
    return gco->gch.tt == LUA_TUSERDATA ? (void*)gco2u(gco)->data : (void*)gco;
}

// An object's size, read from its own fields only (a dead object's references may be freed already)
size_t walksize(GCObject* o)
{
    switch (o->gch.tt)
    {
    case LUA_TSTRING:
        return sizestring(gco2ts(o)->len);
    case LUA_TTABLE:
    {
        LuaTable* h = gco2h(o);
        return sizeof(LuaTable) + (h->node == &luaH_dummynode ? 0 : sizenode(h) * sizeof(LuaNode)) + h->sizearray * sizeof(TValue);
    }
    case LUA_TFUNCTION:
    {
        Closure* cl = gco2cl(o);
        return cl->isC ? sizeCclosure(cl->nupvalues) : sizeLclosure(cl->nupvalues);
    }
    case LUA_TUSERDATA:
        return sizeudata(gco2u(o)->len);
    case LUA_TTHREAD:
    {
        lua_State* th = gco2th(o);
        return sizeof(lua_State) + sizeof(TValue) * th->stacksize + sizeof(CallInfo) * th->size_ci;
    }
    case LUA_TBUFFER:
        return sizebuffer(gco2buf(o)->len);
    case LUA_TPROTO:
    {
        Proto* p = gco2p(o);
        return sizeof(Proto) + sizeof(Instruction) * p->sizecode + sizeof(Proto*) * p->sizep + sizeof(TValue) * p->sizek + p->sizelineinfo +
               sizeof(LocVar) * p->sizelocvars + sizeof(TString*) * p->sizeupvalues;
    }
    case LUA_TUPVAL:
        return sizeof(UpVal);
    }
    return 0;
}

void walknode(HeapWalk* w, GCObject* gco, size_t size, const char* name)
{
    w->node(w->context, walkpointer(gco), gco->gch.tt, gco->gch.memcat, size, name);
}

void walkedge(HeapWalk* w, GCObject* from, GCObject* to, const char* name)
{
    w->edge(w->context, walkpointer(from), walkpointer(to), name);
}

void walkedges(HeapWalk* w, GCObject* from, TValue* data, size_t size, const char* name)
{
    for (size_t i = 0; i < size; ++i)
        if (iscollectable(&data[i]))
            walkedge(w, from, gcvalue(&data[i]), name);
}

void walktable(HeapWalk* w, LuaTable* h)
{
    walknode(w, obj2gco(h), walksize(obj2gco(h)), h == hvalue(registry(w->L)) ? "registry" : NULL);
    bool weakkey = false;
    bool weakvalue = false;
    if (const TValue* mode = gfasttm(w->L->global, h->metatable, TM_MODE))
    {
        if (ttisstring(mode))
        {
            weakkey = strchr(svalue(mode), 'k') != NULL;
            weakvalue = strchr(svalue(mode), 'v') != NULL;
        }
    }
    // (a weak reference is no edge, but a weak table keeps its strings: cleartable never clears a string)
    if (h->node != &luaH_dummynode)
    {
        for (int i = 0; i < sizenode(h); ++i)
        {
            const LuaNode& n = h->node[i];
            if (!ttisnil(&n.val) && (iscollectable(&n.key) || iscollectable(&n.val)))
            {
                if (iscollectable(&n.key) && (!weakkey || ttisstring(&n.key)))
                    walkedge(w, obj2gco(h), gcvalue(&n.key), "[key]");
                if (iscollectable(&n.val) && (!weakvalue || ttisstring(&n.val)))
                {
                    char buf[32];
                    const char* name = buf;
                    if (ttisstring(&n.key))
                        name = svalue(&n.key);
                    else if (ttisnumber(&n.key))
                        snprintf(buf, sizeof(buf), "%.14g", nvalue(&n.key));
                    else
                        snprintf(buf, sizeof(buf), "[%s]", getstr(w->L->global->ttname[n.key.tt]));
                    // (in a table weak in both, a string value stays while its key does: cleartable clears the entry of
                    // a freed key without marking the value)
                    if (weakkey && weakvalue && iscollectable(&n.key) && !ttisstring(&n.key))
                        walkedge(w, gcvalue(&n.key), gcvalue(&n.val), name);
                    else
                        walkedge(w, obj2gco(h), gcvalue(&n.val), name);
                }
            }
        }
    }
    for (int i = 0; i < h->sizearray; ++i)
        if (iscollectable(&h->array[i]) && (!weakvalue || ttisstring(&h->array[i])))
            walkedge(w, obj2gco(h), gcvalue(&h->array[i]), "array");
    if (h->metatable)
        walkedge(w, obj2gco(h), obj2gco(h->metatable), "metatable");
}

void walkclosure(HeapWalk* w, Closure* cl)
{
    if (cl->isC)
    {
        walknode(w, obj2gco(cl), walksize(obj2gco(cl)), cl->c.debugname ? getstr(cl->c.debugname) : NULL);
        // (traverseclosure marks a C function's name)
        if (cl->c.debugname)
            walkedge(w, obj2gco(cl), obj2gco(cl->c.debugname), "name");
    }
    else
    {
        Proto* p = cl->l.p;
        char buf[LUA_IDSIZE];
        if (p->source)
            snprintf(buf, sizeof(buf), "%s:%d %s", p->debugname ? getstr(p->debugname) : "unnamed", p->linedefined, getstr(p->source));
        else
            snprintf(buf, sizeof(buf), "%s:%d", p->debugname ? getstr(p->debugname) : "unnamed", p->linedefined);
        walknode(w, obj2gco(cl), walksize(obj2gco(cl)), buf);
    }
    walkedge(w, obj2gco(cl), obj2gco(cl->env), "env");
    if (cl->isC)
    {
        if (cl->nupvalues)
            walkedges(w, obj2gco(cl), cl->c.upvals, cl->nupvalues, "upvalue");
    }
    else
    {
        walkedge(w, obj2gco(cl), obj2gco(cl->l.p), "proto");
        if (cl->nupvalues)
            walkedges(w, obj2gco(cl), cl->l.uprefs, cl->nupvalues, "upvalue");
    }
}

void walkudata(HeapWalk* w, Udata* u)
{
    const char* name = NULL;
    if (LuaTable* h = u->metatable)
    {
        if (h->node != &luaH_dummynode)
        {
            for (int i = 0; i < sizenode(h); ++i)
            {
                const LuaNode& n = h->node[i];
                if (ttisstring(&n.key) && ttisstring(&n.val) && strcmp(svalue(&n.key), "__type") == 0)
                {
                    name = svalue(&n.val);
                    break;
                }
            }
        }
    }
    walknode(w, obj2gco(u), walksize(obj2gco(u)), name);
    if (u->metatable)
        walkedge(w, obj2gco(u), obj2gco(u->metatable), "metatable");
}

void walkthread(HeapWalk* w, lua_State* th)
{
    size_t size = walksize(obj2gco(th));
    Closure* tcl = NULL;
    for (CallInfo* ci = th->base_ci; ci <= th->ci; ++ci)
    {
        if (ttisfunction(ci->func))
        {
            tcl = clvalue(ci->func);
            break;
        }
    }
    if (tcl && !tcl->isC && tcl->l.p->source)
    {
        Proto* p = tcl->l.p;
        char buf[LUA_IDSIZE];
        snprintf(buf, sizeof(buf), "thread at %s:%d %s", p->debugname ? getstr(p->debugname) : "unnamed", p->linedefined, getstr(p->source));
        walknode(w, obj2gco(th), size, buf);
    }
    else
    {
        walknode(w, obj2gco(th), size, NULL);
    }
    walkedge(w, obj2gco(th), obj2gco(th->gt), "globals");
    if (th->top > th->stack)
        walkedges(w, obj2gco(th), th->stack, th->top - th->stack, "stack");
    // (traversestack marks these too)
    if (th->namecall)
        walkedge(w, obj2gco(th), obj2gco(th->namecall), "namecall");
    if (FFlag::LuauCoroutineFinally && th->finalizers)
        walkedge(w, obj2gco(th), obj2gco(th->finalizers), "finalizers");
    for (UpVal* uv = th->openupval; uv; uv = uv->u.open.threadnext)
        walkedge(w, obj2gco(th), obj2gco(uv), "open upvalue");
}

void walkproto(HeapWalk* w, Proto* p)
{
    size_t size = walksize(obj2gco(p));
    char buf[LUA_IDSIZE];
    if (p->source)
        snprintf(buf, sizeof(buf), "proto %s:%d %s", p->debugname ? getstr(p->debugname) : "unnamed", p->linedefined, getstr(p->source));
    else
        snprintf(buf, sizeof(buf), "proto %s:%d", p->debugname ? getstr(p->debugname) : "unnamed", p->linedefined);
    walknode(w, obj2gco(p), size, buf);
    if (p->sizek)
        walkedges(w, obj2gco(p), p->k, p->sizek, "constants");
    for (int i = 0; i < p->sizep; ++i)
        walkedge(w, obj2gco(p), obj2gco(p->p[i]), "protos");
    // (traverseproto marks the names too)
    if (p->source)
        walkedge(w, obj2gco(p), obj2gco(p->source), "source");
    if (p->debugname)
        walkedge(w, obj2gco(p), obj2gco(p->debugname), "debugname");
    for (int i = 0; i < p->sizeupvalues; ++i)
        if (p->upvalues[i])
            walkedge(w, obj2gco(p), obj2gco(p->upvalues[i]), "upvalue name");
    for (int i = 0; i < p->sizelocvars; ++i)
        if (p->locvars[i].varname)
            walkedge(w, obj2gco(p), obj2gco(p->locvars[i].varname), "local name");
    if (p->optimized)
        walkedge(w, obj2gco(p), obj2gco(p->optimized), "optimized");
    if (p->deoptimized)
        walkedge(w, obj2gco(p), obj2gco(p->deoptimized), "deoptimized");
}

void walkobject(HeapWalk* w, GCObject* o)
{
    switch (o->gch.tt)
    {
    case LUA_TSTRING:
        return walknode(w, o, walksize(o), NULL);
    case LUA_TTABLE:
        return walktable(w, gco2h(o));
    case LUA_TFUNCTION:
        return walkclosure(w, gco2cl(o));
    case LUA_TUSERDATA:
        return walkudata(w, gco2u(o));
    case LUA_TTHREAD:
        return walkthread(w, gco2th(o));
    case LUA_TBUFFER:
        return walknode(w, o, walksize(o), NULL);
    case LUA_TPROTO:
        return walkproto(w, gco2p(o));
    case LUA_TUPVAL:
    {
        UpVal* uv = gco2uv(o);
        walknode(w, o, walksize(o), NULL);
        if (iscollectable(uv->v))
            walkedge(w, o, gcvalue(uv->v), "value");
        return;
    }
    default:
        // (a type the walk does not open: user-defined classes and objects)
        return walknode(w, o, walksize(o), NULL);
    }
}

bool walkvisit(void* context, lua_Page* page, GCObject* gco)
{
    HeapWalk* w = (HeapWalk*)context;
    // (dead, not swept yet: garbage whose references may already be freed)
    if (isdead(w->L->global, gco))
    {
        if (w->dead)
            w->dead(w->context, gco->gch.tt, walksize(gco));
        return false;
    }
    walkobject(w, gco);
    // (the collector never frees a fixed object: the type and metamethod names, the memory error messages)
    if (isfixed(gco))
        w->edge(w->context, NULL, walkpointer(gco), "fixed");
    return false;
}
} // namespace

void lua_enumheap(
    lua_State* L,
    void* context,
    void (*node)(void* context, void* ptr, uint8_t tt, uint8_t memcat, size_t size, const char* name),
    void (*edge)(void* context, void* from, void* to, const char* name),
    void (*dead)(void* context, uint8_t tt, size_t size)
)
{
    HeapWalk w = {L, context, node, edge, dead};
    global_State* g = L->global;
    walkobject(&w, obj2gco(g->mainthread));
    // the roots markroot and atomic mark
    edge(context, NULL, walkpointer(obj2gco(g->mainthread)), "main thread");
    if (iscollectable(registry(L)))
        edge(context, NULL, walkpointer(gcvalue(registry(L))), "registry");
    for (int i = 0; i < LUA_T_COUNT; ++i)
    {
        if (g->mt[i])
        {
            char buf[64];
            snprintf(buf, sizeof(buf), "metatable of %s", getstr(g->ttname[i]));
            edge(context, NULL, walkpointer(obj2gco(g->mt[i])), buf);
        }
    }
    // (the rest of markroot's and atomic's roots, under the flags that mark them)
    if (FFlag::LuauGcTraceUdata && iscollectable(&g->weakregistry))
        edge(context, NULL, walkpointer(gcvalue(&g->weakregistry)), "weak registry");
    for (int i = 0; i < LUA_UTAG_LIMIT; ++i)
        if (g->udatamt[i])
            edge(context, NULL, walkpointer(obj2gco(g->udatamt[i])), "userdata metatable");
    for (int i = 0; i < UTAG_INTERNAL_LIMIT; ++i)
    {
        const lua_UdataDirectAccessData& direct = g->udatadirect[i];
        for (const TValue* tm : {&direct.indextm, &direct.newindextm, &direct.namecalltm})
            if (iscollectable(tm))
                edge(context, NULL, walkpointer(gcvalue(tm)), "userdata direct access");
        if (g->udatadirectfields[i])
            edge(context, NULL, walkpointer(obj2gco(g->udatadirectfields[i])), "userdata direct fields");
    }
    if (FFlag::LuauFastpcall)
    {
        if (g->builtinPcall)
            edge(context, NULL, walkpointer(obj2gco(g->builtinPcall)), "pcall");
        if (g->builtinXpcall)
            edge(context, NULL, walkpointer(obj2gco(g->builtinXpcall)), "xpcall");
    }
    if (g->typeofnames)
        edge(context, NULL, walkpointer(obj2gco(g->typeofnames)), "typeof names");
    if (g->vectortypename)
        edge(context, NULL, walkpointer(obj2gco(g->vectortypename)), "typeof names");
    luaM_visitgco(L, &w, walkvisit);
}

int lua_gcstate(lua_State* L)
{
    return L->global->gcstate;
}
