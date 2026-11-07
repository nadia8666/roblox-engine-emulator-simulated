// This file is part of the Luau programming language and is licensed under MIT License; see LICENSE.txt for details
#pragma once

#include "Luau/Common.h"

#include <mutex>
#include <stdlib.h>
#include <string.h>
#include <string>

// The flag values a live Roblox client runs this release of Luau with (ClientFlagValues.h, which the engine's
// tools/vendor.mjs writes from its data/luau-flags.json), set once before the first VM starts or the first chunk
// compiles: lua_newstate and luau_compile call applyClientFlags. A flag the client does not serve keeps its default.
// REE_LUAU_FLAGS changes them for an experiment: `off` keeps every default, and `Name=value,...` (Roblox's names:
// FFlag, DFFlag, FInt or DFInt and the flag; true / false or a number) sets those on top of the client's.
namespace Luau
{

struct ClientFlagValue
{
    const char* name; // as Luau registers it, without the prefix
    bool dynamic;
    bool isint;
    int value;
};

inline bool setClientFlag(const char* name, bool dynamic, bool isint, int value)
{
    if (isint)
    {
        for (FValue<int>* flag = FValue<int>::list; flag; flag = flag->next)
            if (flag->dynamic == dynamic && strcmp(flag->name, name) == 0)
            {
                flag->value = value;
                return true;
            }
    }
    else
    {
        for (FValue<bool>* flag = FValue<bool>::list; flag; flag = flag->next)
            if (flag->dynamic == dynamic && strcmp(flag->name, name) == 0)
            {
                flag->value = value != 0;
                return true;
            }
    }
    return false;
}

// one REE_LUAU_FLAGS item, `DFFlagLuauX=true` or `FIntLuauY=3`: its kind is its prefix
inline void setNamedClientFlag(const std::string& item)
{
    struct Prefix
    {
        const char* text;
        bool dynamic;
        bool isint;
    };
    static const Prefix prefixes[] = {{"DFFlag", true, false}, {"DFInt", true, true}, {"FFlag", false, false}, {"FInt", false, true}};

    size_t equals = item.find('=');
    if (equals == std::string::npos)
        return;
    for (const Prefix& prefix : prefixes)
    {
        size_t length = strlen(prefix.text);
        if (equals > length && item.compare(0, length, prefix.text) == 0)
        {
            std::string name = item.substr(length, equals - length);
            std::string value = item.substr(equals + 1);
            int parsed = prefix.isint ? atoi(value.c_str()) : (value == "true" || value == "True" || value == "1");
            setClientFlag(name.c_str(), prefix.dynamic, prefix.isint, parsed);
            return;
        }
    }
}

inline void applyClientFlags()
{
    static std::once_flag once;
    std::call_once(
        once,
        []
        {
            static const ClientFlagValue values[] = {
#define LUAU_CLIENT_FLAG(name, dynamic, isint, value) {#name, dynamic, isint, value},
#include "Luau/ClientFlagValues.h"
#undef LUAU_CLIENT_FLAG
                {nullptr, false, false, 0},
            };

            const char* overrides = getenv("REE_LUAU_FLAGS");
            bool off = overrides && strcmp(overrides, "off") == 0;
            if (!off)
                for (const ClientFlagValue* v = values; v->name; ++v)
                    setClientFlag(v->name, v->dynamic, v->isint, v->value);

            if (overrides && !off)
            {
                std::string list = overrides;
                size_t start = 0;
                while (start <= list.size())
                {
                    size_t comma = list.find(',', start);
                    size_t end = comma == std::string::npos ? list.size() : comma;
                    setNamedClientFlag(list.substr(start, end - start));
                    start = end + 1;
                }
            }
        }
    );
}

} // namespace Luau
