//! ReflectionService:GetClass and GetClasses from the compiled-in API dump (crate::reflection), as a game script gets them.
//!
//! Measured in roblox-sim (Studio 0.744): a playing client's game script and a server's see the same classes, every class of
//! the API dump (a plugin context's list) and none of the ones only rbx-dom describes (GetClass("Player"), GetClass("CoreGui")
//! and GetClass("UserGameSettings") are nil). GetClasses lists them in name order, each call a new frozen list of new frozen
//! entries: `Name`, `Superclass` (absent for Object), `Serialized` (true, or absent), `Subclasses` (every subclass in name
//! order, those a script cannot get included; absent when there is none), `Display` (Category and, where the class has one,
//! DeprecationMessage) and `Permits` (GetService / New, SecurityCapabilities). The filter is a dictionary: `IsA` (the class and
//! its subclasses; a name no class has gives none), `ExcludeDisplay` (no Display), `Security`; other keys are ignored.
//! ree emulates no SecurityCapabilities, so `Permits` is a frozen empty table that reports itself as not emulated when a script
//! reads or iterates it (Output `missing`, ReflectedClass.Permits). A `Security` filter of another type is refused as Roblox
//! refuses it; a SecurityCapabilities one (ree has no such value yet) would report itself (ReflectionClassFilter.Security) and
//! filter nothing. GetMethodsOfClass and GetEventsOfClass need fields the dump does not record (a parameter's DefaultValue,
//! member Display and event Permits) and stay unemulated. GetPropertiesOfClass is approximate (not measured): each entry has
//! only `Name`, since the dump lacks a reference type's InstanceType and the member Display; it lists every property a script
//! can read, inherited ones included, in name order, and an unknown class gives an empty list.

use std::rc::Rc;

use mlua::prelude::*;

use super::{Behaviors, method, values};
use crate::{
    ClassId, Engine,
    lua::{Args, Flow, raise, typeof_value},
};

/// The registry key of the Permits stand-in's metatable.
const PERMITS_META: &str = "ree.ReflectedClass.Permits";

/// A ReflectionClassFilter, checked.
struct Filter {
    /// `IsA`: the class entries must derive from; `Some(None)` names no class (nothing matches)
    is_a: Option<Option<ClassId>>,
    display: bool,
}

/// A value's name in a failed cast ("Unable to cast double to Dictionary"), as the engine names it.
fn cast_name(engine: &Engine, value: &LuaValue) -> String {
    match value {
        LuaValue::Number(_) | LuaValue::Integer(_) => "double".into(),
        LuaValue::Boolean(_) => "bool".into(),
        LuaValue::Table(table) if table.raw_len() > 0 => "Array".into(),
        other => typeof_value(engine, other),
    }
}

/// The filter argument at `index`: nil or a dictionary (measured: a number "Unable to cast double to Dictionary", `{ "BasePart" }` and
/// `{ 1, IsA = "BasePart" }` "Unable to cast Array to Dictionary"; a field of the wrong type "ReflectionClassFilter's IsA field is
/// expected to be type string", "... ExcludeDisplay field is expected to be type bool").
fn filter(engine: &Engine, args: &Args, index: usize) -> Result<Filter, Flow> {
    let table = match args.get(index) {
        LuaValue::Nil => return Ok(Filter { is_a: None, display: true }),
        LuaValue::Table(table) if table.raw_len() == 0 => table.clone(),
        other => return raise(format!("Unable to cast {} to Dictionary", cast_name(engine, other))),
    };
    let is_a = match table.raw_get::<LuaValue>("IsA")? {
        LuaValue::Nil => None,
        LuaValue::String(name) => Some(engine.reflection.class_id(&name.to_string_lossy())),
        _ => return raise("ReflectionClassFilter's IsA field is expected to be type string"),
    };
    let display = match table.raw_get::<LuaValue>("ExcludeDisplay")? {
        LuaValue::Nil => true,
        LuaValue::Boolean(exclude) => !exclude,
        _ => return raise("ReflectionClassFilter's ExcludeDisplay field is expected to be type bool"),
    };
    // (a value of another type is refused, as measured; ree has no SecurityCapabilities to filter by, so one reports itself)
    match table.raw_get::<LuaValue>("Security")? {
        LuaValue::Nil => {}
        value if typeof_value(engine, &value) == "SecurityCapabilities" => {
            engine.state.borrow_mut().output.missing("ReflectionClassFilter", "Security");
        }
        _ => return raise("ReflectionClassFilter's Security field is expected to be type SecurityCapabilities"),
    }
    Ok(Filter { is_a, display })
}

fn matches(engine: &Engine, class: ClassId, filter: &Filter) -> bool {
    let info = engine.reflection.class(class);
    info.reflected
        && match filter.is_a {
            None => true,
            Some(None) => false,
            Some(Some(ancestor)) => info.is_a(ancestor),
        }
}

/// The Permits stand-in's metatable: a read of any key, or an iteration, reports ReflectedClass.Permits as not emulated.
fn permits_meta(lua: &Lua) -> LuaResult<LuaTable> {
    if let Ok(LuaValue::Table(meta)) = lua.named_registry_value::<LuaValue>(PERMITS_META) {
        return Ok(meta);
    }
    fn report(lua: &Lua) {
        if let Some(engine) = lua.app_data_ref::<Rc<Engine>>() {
            engine.state.borrow_mut().output.missing("ReflectedClass", "Permits");
        }
    }
    let meta = lua.create_table()?;
    meta.raw_set("__index", lua.create_function(|lua, _: LuaMultiValue| {
        report(lua);
        Ok(LuaValue::Nil)
    })?)?;
    let done = lua.create_function(|_, _: LuaMultiValue| Ok(LuaValue::Nil))?;
    meta.raw_set("__iter", lua.create_function(move |lua, _: LuaMultiValue| {
        report(lua);
        Ok(done.clone())
    })?)?;
    meta.set_readonly(true);
    lua.set_named_registry_value(PERMITS_META, meta.clone())?;
    Ok(meta)
}

fn frozen(table: LuaTable) -> LuaTable {
    table.set_readonly(true);
    table
}

/// A class's ReflectedClass entry: a new frozen table, as each call makes.
fn entry(lua: &Lua, engine: &Engine, class: ClassId, display: bool) -> LuaResult<LuaTable> {
    let reflection = &engine.reflection;
    let info = reflection.class(class);
    let entry = lua.create_table_with_capacity(0, 6)?;
    entry.raw_set("Name", &*info.name)?;
    if let Some(superclass) = info.superclass {
        entry.raw_set("Superclass", &*reflection.class(superclass).name)?;
    }
    if info.serialized {
        entry.raw_set("Serialized", true)?;
    }
    if !info.subclasses.is_empty() {
        let list = lua.create_table_with_capacity(info.subclasses.len(), 0)?;
        for (index, &subclass) in info.subclasses.iter().enumerate() {
            list.raw_set(index + 1, &*reflection.class(subclass).name)?;
        }
        entry.raw_set("Subclasses", frozen(list))?;
    }
    if display {
        let table = lua.create_table_with_capacity(0, info.display.len())?;
        for (key, text) in &info.display {
            table.raw_set(&**key, &**text)?;
        }
        entry.raw_set("Display", frozen(table))?;
    }
    let permits = lua.create_table()?;
    permits.set_metatable(Some(permits_meta(lua)?))?;
    entry.raw_set("Permits", frozen(permits))?;
    Ok(frozen(entry))
}

/// The work clock's price of what a call made and tested (cost.rs REFLECTED_CLASS).
fn charge(engine: &Engine, made: usize, tested: usize, display: bool) {
    let each = crate::cost::REFLECTED_CLASS + if display { crate::cost::REFLECTED_CLASS_DISPLAY } else { 0.0 };
    let seconds = made as f64 * each + tested as f64 * crate::cost::REFLECTED_CLASS_TESTED;
    engine.state.borrow().sched.charge(seconds);
    crate::boundary::add_cost(seconds);
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let Some(service) = reflection.class_id("ReflectionService") else { return };
    if let Some(n) = reflection.name_id("GetClasses") {
        behaviors.add_method(service, n, method(|lua, engine, _, args| {
            let filter = filter(engine, args, 1)?;
            let classes = &engine.reflection.classes;
            let mut found: Vec<ClassId> = classes.iter().map(|class| class.id).filter(|&class| matches(engine, class, &filter)).collect();
            found.sort_unstable_by(|&a, &b| classes[a as usize].name.cmp(&classes[b as usize].name));
            let list = lua.create_table_with_capacity(found.len(), 0)?;
            for (index, &class) in found.iter().enumerate() {
                list.raw_set(index + 1, entry(lua, engine, class, filter.display)?)?;
            }
            charge(engine, found.len(), classes.iter().filter(|class| class.reflected).count(), filter.display);
            Ok(values([LuaValue::Table(frozen(list))]))
        }));
    }
    if let Some(n) = reflection.name_id("GetClass") {
        behaviors.add_method(service, n, method(|lua, engine, _, args| {
            // (measured: nil "Argument 2 missing or nil"; a number is looked up as its text; another value finds no class)
            let name = match args.get(1) {
                LuaValue::Nil => return raise("Argument 2 missing or nil"),
                LuaValue::String(name) => Some(name.to_string_lossy()),
                LuaValue::Number(_) | LuaValue::Integer(_) => lua.coerce_string(args.get(1).clone())?.map(|text| text.to_string_lossy()),
                _ => None,
            };
            let filter = filter(engine, args, 2)?;
            let class = name.and_then(|name| engine.reflection.class_id(&name)).filter(|&class| matches(engine, class, &filter));
            let Some(class) = class else { return Ok(values([LuaValue::Nil])) };
            charge(engine, 1, 0, filter.display);
            Ok(values([LuaValue::Table(entry(lua, engine, class, filter.display)?)]))
        }));
    }
    if let Some(n) = reflection.name_id("GetPropertiesOfClass") {
        behaviors.add_method(service, n, method(|lua, engine, _, args| {
            let name = match args.get(1) {
                LuaValue::Nil => return raise("Argument 2 missing or nil"),
                LuaValue::String(name) => Some(name.to_string_lossy()),
                _ => None,
            };
            let class = name.and_then(|name| engine.reflection.class_id(&name));
            let list = lua.create_table()?;
            if let Some(class) = class {
                let reflection = &engine.reflection;
                let mut names: Vec<&str> = reflection
                    .class(class)
                    .properties
                    .values()
                    .map(|&index| reflection.property(index))
                    .filter(|property| property.scriptable)
                    .map(|property| reflection.name(property.name))
                    .collect();
                names.sort_unstable();
                for (index, property_name) in names.iter().enumerate() {
                    let entry = lua.create_table_with_capacity(0, 1)?;
                    entry.raw_set("Name", *property_name)?;
                    list.raw_set(index + 1, frozen(entry))?;
                }
                charge(engine, names.len(), 0, false);
            }
            Ok(values([LuaValue::Table(frozen(list))]))
        }));
    }
}
