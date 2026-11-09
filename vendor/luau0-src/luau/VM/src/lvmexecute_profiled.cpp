// This file is part of the Luau programming language and is licensed under MIT License; see LICENSE.txt for details
// This code is based on Lua 5.x implementation licensed under MIT License; see lua_LICENSE.txt for details

// The interpreter's copy that samples calls for a host's profile (luau_execute<false, true>: lprofile.cpp), in a
// translation unit of its own so that it compiles beside the plain copy rather than after it
#define LUAU_EXECUTE_PROFILED_UNIT 1
#include "lvmexecute.cpp"
