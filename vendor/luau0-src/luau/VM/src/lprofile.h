// This file is part of the Luau programming language and is licensed under MIT License; see LICENSE.txt for details
#pragma once

#include "lobject.h"

// A host's profile of this thread's VM (lprofile.cpp, lua.h "profiling"). Off, the interpreter pays one thread-local load
// per luau_execute entry, and the VM one test of luaP_state where a C function or the host (no script instruction) allocates
// an object or table storage or rehashes a table (luaP_uncounted's sites in lmem.cpp and ltable.cpp), at a table's first
// touch in a frame while cold touches are tracked (13-cold-touches), at a chunk's load, at an error a protected call catches
// and where a thread is freed or reset; a storage allocation also stores what it is for (luaP_allockind).
struct lua_ProfileState;

extern thread_local lua_ProfileState* luaP_state;

// set right before a storage allocation (a table's array or hash part, a thread's stack, the string table, a function's
// prototype arrays) and read by the allocation callback (lua_allocblock): what the memory is for; only the store when no
// profile runs. Both are the allocating thread's, as luaP_state is: VMs on other threads allocate at the same time, and a
// callback that read another thread's block would read, and mark, an object of that VM's heap.
extern thread_local uint8_t luaP_allockind;
// the collectable object an allocation callback reports (NULL for other memory)
extern thread_local void* luaP_allocblock;

// whether L's VM runs the stepping interpreter for a profile (luau_execute)
LUAI_FUNC bool luaP_stepping(lua_State* L);
// whether native code runs natively while the stepping interpreter counts the rest
LUAI_FUNC bool luaP_native(lua_State* L);
// the stepping interpreter's hook, before each instruction it dispatches
LUAI_FUNC void luaP_step(lua_State* L, Closure* cl, const Instruction* pc);
// native code is entered from C or resumed by a return into it: its work is booked to it until the next step
LUAI_FUNC void luaP_nativeenter(lua_State* L, Closure* cl);
// the interpreter was entered or left (a resumption, a call from C), while a profile runs: `frame` is where its luau_execute
// frame sits on the C stack
LUAI_FUNC void luaP_enter(lua_State* L, const void* frame);
LUAI_FUNC void luaP_leave(lua_State* L, const void* frame);
// an error was caught (luaD_rawrunprotected) at `frame` on the C stack: the interpreter's entries deeper than it are gone (a
// build without unwind semantics, as ree's, skips luaP_Execute's destructor)
LUAI_FUNC void luaP_unwound(lua_State* L, const void* frame);
// a chunk was loaded (lvmload.cpp): its functions are registered, and the copies its compiler inlined found
LUAI_FUNC void luaP_loaded(lua_State* L, Proto* main);
// work the counters leave to a call (a C function's or the host's Luau's, running for a script): the objects it allocates, its
// table storage bytes and the keys its tables' rehashes move, which a builtin's fast path counts as its own; a profile counts it
// toward what a declined builtin call would charge on the fast path
LUAI_FUNC void luaP_uncounted(lua_State* L, int objects, size_t bytes, int keys);
// a script's work touched the table `o` first in the frame (lgc.cpp's cold touches, counted in lua_WorkCounters' coldtables): the
// table an instruction with an inline cache reaches first has the charge told apart (lua_ProfileCacheSite.firsttouch)
LUAI_FUNC void luaP_coldtouch(lua_State* L, GCObject* o);
// a thread was freed or reset: its frames go
LUAI_FUNC void luaP_threadgone(lua_State* L, lua_State* L1);
// an object marked with lua_profilemark is being freed
LUAI_FUNC void luaP_freed(lua_State* L, GCObject* o);
// a table rehashed to fit a new key (ltable.cpp rehash, once resized): its array and hash sizes before, and the keys it
// moved with the new one (what the work counters' `rehashed` counts); the profile's rehash hook (lua_setprofilerehash)
LUAI_FUNC void luaP_rehash(lua_State* L, LuaTable* t, int oldarray, int oldnodes, int keys);

// while a profile's call hook is set (lua_setprofilecalls, global_State::profilecalls) the interpreter calls
// luaP_callenter as each Luau call's frame is set up: luau_execute then runs an instantiation of its own that asks, so the
// one that runs without a profile does not (a call from C or a metamethod's fast path loads the flag)
struct CallInfo;
// a Luau call's frame (L->ci, of L->ci->p) is set up and its first instruction is next: the hook when the call is
// sampled (only L passed: the interpreter keeps nothing more live for a call it almost never makes)
LUAI_FUNC void luaP_callenter(lua_State* L);
// a sampled call (LUA_CALLINFO_PROFILECALL) returns, its frame still in place
LUAI_FUNC void luaP_callleave(lua_State* L, CallInfo* ci);
// an error unwinds L's frames above `to` (a protected call's recovery), or all of them (an error ended the thread, or it
// is reset: L->base_ci): each sampled call among them leaves, innermost first
LUAI_FUNC void luaP_callsunwound(lua_State* L, CallInfo* to);
// the interpreter's copy that samples calls (lvmexecute_profiled.cpp, a translation unit of its own so it compiles beside
// the plain one): luau_execute runs it while a call hook is set
LUAI_FUNC void luaP_executeprofiled(lua_State* L);

// luau_execute holds one while it runs under a profile (an error that unwinds it may skip its destructor: the catch tells the
// profile, luaP_unwound)
struct luaP_Execute
{
    lua_State* L;
    explicit luaP_Execute(lua_State* L)
        : L(L)
    {
        luaP_enter(L, this);
    }
    ~luaP_Execute()
    {
        if (luaP_state)
            luaP_leave(L, this);
    }
};
