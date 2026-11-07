// This file is part of the Luau programming language and is licensed under MIT License; see LICENSE.txt for details
#pragma once

#include "lobject.h"

// A host's profile of this thread's VM (lprofile.cpp, lua.h "profiling"). Off, the interpreter pays one thread-local load
// per luau_execute entry; the allocator and the collector pay nothing beyond the callbacks they already make.
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
// the stepping interpreter was entered or left (a resumption, a call from C)
LUAI_FUNC void luaP_enter(lua_State* L);
LUAI_FUNC void luaP_leave(lua_State* L);
// a thread was freed or reset: its frames go
LUAI_FUNC void luaP_threadgone(lua_State* L, lua_State* L1);
// an object marked with lua_profilemark is being freed
LUAI_FUNC void luaP_freed(lua_State* L, GCObject* o);
// a table rehashed to fit a new key (ltable.cpp rehash, once resized): its array and hash sizes before, and the keys it
// moved with the new one (what the work counters' `rehashed` counts); the profile's rehash hook (lua_setprofilerehash)
LUAI_FUNC void luaP_rehash(lua_State* L, LuaTable* t, int oldarray, int oldnodes, int keys);

// luau_execute<true> holds one while it runs (its destructor also runs when an error unwinds it)
struct luaP_Execute
{
    lua_State* L;
    explicit luaP_Execute(lua_State* L)
        : L(L)
    {
        if (luaP_state)
            luaP_enter(L);
    }
    ~luaP_Execute()
    {
        if (luaP_state)
            luaP_leave(L);
    }
};

struct luaP_NoExecute
{
    explicit luaP_NoExecute(lua_State*) {}
};
