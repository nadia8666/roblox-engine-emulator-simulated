//! Instance query selectors (port of `runtime/engine/query.luau`): Instance:QueryDescendants (Studio has
//! no QueryFirstDescendant: it is not a member) with class (IsA), #Name, .Tag, [Prop = v], [$Attribute = v], the
//! combinators " ", ">>", ">", selector lists and :not(...) / :has(...).

use std::rc::Rc;

use mlua::prelude::*;
use rustc_hash::FxHashMap;
use std::cell::RefCell;

use super::{Behaviors, method, values};
use crate::{
    Engine, InstanceId,
    lua::{Flow, instance::get, instance_value, raise_plain, typeof_value},
    project,
};

#[derive(Debug, Default)]
struct Compound {
    /// Every class filter must hold (a space between filters does not start a new step: "Part Part" is one)
    classes: Vec<String>,
    names: Vec<String>,
    tags: Vec<String>,
    properties: Vec<(String, bool, Option<String>)>,
    not: Vec<Vec<Selector>>,
    has: Vec<Vec<Selector>>,
}

#[derive(Debug)]
struct Step {
    child_only: bool,
    compound: Compound,
}

#[derive(Debug)]
struct Selector {
    steps: Vec<Step>,
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_name(byte: u8) -> bool {
    is_word(byte) || byte == b'-'
}

fn string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Studio's selector grammar (measured 2026-10-23 in an empty test place): whitespace is never a combinator, only `>` (a
/// child) and `>>` (a descendant) join steps, whatever surrounds them; filters separated by spaces all apply
/// to the same instance ("Model :has(> Part)", "Part #X"); a selector list answers each selector in turn
/// (tree order within it, duplicates kept); an empty query answers nothing. Malformed queries raise the
/// engine's messages.
struct Parser<'a> {
    text: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.position).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }

    fn run(&mut self, predicate: impl Fn(u8) -> bool) -> &[u8] {
        let start = self.position;
        while self.peek().is_some_and(&predicate) {
            self.position += 1;
        }
        &self.text[start..self.position]
    }

    /// Selectors up to the end of the text or a `)`.
    fn list(&mut self) -> Result<Vec<Selector>, String> {
        let mut selectors = vec![self.selector()?];
        self.skip_space();
        while self.peek() == Some(b',') {
            self.position += 1;
            selectors.push(self.selector()?);
            self.skip_space();
        }
        Ok(selectors)
    }

    fn selector(&mut self) -> Result<Selector, String> {
        let mut steps: Vec<Step> = Vec::new();
        let mut pending: Option<bool> = None;
        loop {
            self.skip_space();
            if self.peek() == Some(b'>') {
                if pending.is_some() {
                    return Err("Expected at least one filter".into());
                }
                if self.text[self.position..].starts_with(b">>") {
                    self.position += 2;
                    pending = Some(false);
                } else {
                    self.position += 1;
                    pending = Some(true);
                }
                continue;
            }
            if !self.peek().is_some_and(|byte| is_word(byte) || b"#.[:".contains(&byte)) {
                if pending.is_some() {
                    return Err("Expected at least one filter".into());
                }
                if steps.is_empty() {
                    return Err(if self.peek().is_none() { "Expected at least one combinator or a filter" } else { "Expected at least one filter" }.into());
                }
                return Ok(Selector { steps });
            }
            if pending.is_some() || steps.is_empty() {
                steps.push(Step { child_only: pending.take().unwrap_or(false), compound: Compound::default() });
            }
            let compound = &mut steps.last_mut().expect("a step").compound;
            self.filter(compound)?;
        }
    }

    fn filter(&mut self, compound: &mut Compound) -> Result<(), String> {
        match self.peek() {
            Some(b'#') => {
                self.position += 1;
                let name = self.run(is_name);
                if name.is_empty() {
                    return Err("Expected identifier for a name".into());
                }
                compound.names.push(string(name));
            }
            Some(b'.') => {
                self.position += 1;
                let tag = self.run(is_name);
                if tag.is_empty() {
                    return Err("Expected identifier for a tag name".into());
                }
                compound.tags.push(string(tag));
            }
            Some(b'[') => {
                self.position += 1;
                self.skip_space();
                let attribute = self.peek() == Some(b'$');
                if attribute {
                    self.position += 1;
                }
                let name = string(self.run(is_word));
                if name.is_empty() {
                    return Err("Expected identifier for a property name".into());
                }
                self.skip_space();
                let mut value = None;
                // (an attribute may stand alone: it is tested for existing)
                if !(attribute && self.peek() == Some(b']')) {
                    if self.peek() != Some(b'=') {
                        return Err("comparison operator expected after property name".into());
                    }
                    self.position += 1;
                    self.skip_space();
                    value = Some(if self.peek() == Some(b'"') {
                        self.position += 1;
                        let quoted = string(self.run(|byte| byte != b'"'));
                        if self.peek() != Some(b'"') {
                            return Err("Expected a property value".into());
                        }
                        self.position += 1;
                        quoted
                    } else {
                        let bare = string(self.run(|byte| is_name(byte) || byte == b'.'));
                        if bare.is_empty() {
                            return Err("Expected a property value".into());
                        }
                        bare
                    });
                    self.skip_space();
                }
                if self.peek() != Some(b']') {
                    return Err("Expected ']' to complete a property filter".into());
                }
                self.position += 1;
                compound.properties.push((name, attribute, value));
            }
            Some(b':') => {
                self.position += 1;
                let pseudo = string(self.run(is_name));
                if pseudo.is_empty() {
                    return Err("Expected identifier for a pseudo-class name".into());
                }
                if pseudo != "not" && pseudo != "has" {
                    return Err(format!("Pseudo-class '{pseudo}' filter is not supported"));
                }
                if self.peek() != Some(b'(') {
                    return Err(format!("Expected '(' after pseudo-class '{pseudo}'"));
                }
                self.position += 1;
                let list = self.list()?;
                if self.peek() != Some(b')') {
                    return Err("Expected ')' to complete a pseudo-class filter".into());
                }
                self.position += 1;
                if pseudo == "not" { compound.not.push(list) } else { compound.has.push(list) }
            }
            _ => {
                let class = self.run(is_word);
                compound.classes.push(string(class));
            }
        }
        Ok(())
    }
}

/// The selectors of a query; an empty (or blank) query has none.
fn parse_list(text: &[u8]) -> Result<Vec<Selector>, String> {
    if text.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }
    let mut parser = Parser { text, position: 0 };
    let list = parser.list()?;
    match parser.peek() {
        None => Ok(list),
        Some(byte) => Err(format!("Unexpected trailing character: '{}'", byte as char)),
    }
}

struct Matcher<'a> {
    lua: &'a Lua,
    engine: &'a Rc<Engine>,
}

impl Matcher<'_> {
    /// `property` names the property being compared (an attribute has none): Studio compares strings,
    /// numbers, booleans (by their lower-case names) and enums, and raises for any other type.
    fn value_matches(&self, actual: &LuaValue, expected: Option<&str>, property: Option<&str>) -> LuaResult<bool> {
        let Some(expected) = expected else {
            return Ok(!matches!(actual, LuaValue::Nil | LuaValue::Boolean(false)));
        };
        Ok(match actual {
            LuaValue::Number(n) => self.lua.coerce_number(LuaValue::String(self.lua.create_string(expected)?))? == Some(*n),
            LuaValue::Integer(n) => self.lua.coerce_number(LuaValue::String(self.lua.create_string(expected)?))? == Some(*n as f64),
            LuaValue::Boolean(b) => b.to_string() == expected,
            LuaValue::String(s) => s.as_bytes() == expected.as_bytes(),
            other => {
                if typeof_value(self.engine, other) == "EnumItem" {
                    let LuaValue::UserData(ud) = other else { return Ok(false) };
                    let item = ud.borrow::<crate::lua::types::LEnumItem>()?.0;
                    let info = self.engine.reflection.enum_item_info(item);
                    return Ok(&*info.name == expected || self.engine.reflection.enum_item_path(item) == expected);
                }
                if let Some(property) = property {
                    if !matches!(other, LuaValue::Nil) {
                        return Err(LuaError::runtime(format!("Property {property} of type {} is not supported for comparison", typeof_value(self.engine, other))));
                    }
                    return Ok(false);
                }
                let tostring: LuaFunction = self.lua.globals().get("tostring")?;
                tostring.call::<String>(other.clone())? == expected
            }
        })
    }

    fn compound_matches(&self, id: InstanceId, compound: &Compound) -> LuaResult<bool> {
        let (class, name_ok, tags_ok) = {
            let state = self.engine.state.borrow();
            let instance = state.dom.at(id);
            let class_ok = compound.classes.iter().all(|c| self.engine.reflection.is_a(instance.class, c));
            let name_ok = compound.names.iter().all(|n| instance.name.as_bytes() == n.as_bytes());
            let tags_ok = compound.tags.iter().all(|t| instance.tags.iter().any(|x| x.as_bytes() == t.as_bytes()));
            (class_ok, name_ok, tags_ok)
        };
        if !class || !name_ok || !tags_ok {
            return Ok(false);
        }
        for (name, attribute, value) in &compound.properties {
            let actual = if *attribute {
                let stored = self.engine.state.borrow().dom.at(id).attribute(name.as_bytes()).cloned();
                match stored {
                    Some(value) => crate::lua::convert::to_lua(self.lua, self.engine, &value)?,
                    None => LuaValue::Nil,
                }
            } else {
                match get(self.lua, self.engine, id, &LuaValue::String(self.lua.create_string(name)?)) {
                    Ok(value) => value,
                    Err(_) => return Ok(false),
                }
            };
            if !self.value_matches(&actual, value.as_deref(), (!*attribute).then_some(name.as_str()))? {
                return Ok(false);
            }
        }
        for list in &compound.not {
            if self.matches_list(id, list, None)? {
                return Ok(false);
            }
        }
        for list in &compound.has {
            if !self.has_match(id, list)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn parent(&self, id: InstanceId) -> Option<InstanceId> {
        self.engine.state.borrow().dom.at(id).parent
    }

    fn match_from(&self, node: InstanceId, steps: &[Step], index: usize, scope: Option<InstanceId>) -> LuaResult<bool> {
        let step = &steps[index];
        if !self.compound_matches(node, &step.compound)? {
            return Ok(false);
        }
        let parent = self.parent(node);
        if index == 0 {
            let Some(scope) = scope else { return Ok(true) };
            if step.child_only {
                return Ok(parent == Some(scope));
            }
            let mut current = parent;
            while let Some(c) = current {
                if c == scope {
                    return Ok(true);
                }
                current = self.parent(c);
            }
            return Ok(false);
        }
        if step.child_only {
            return match parent {
                Some(p) if Some(p) != scope => self.match_from(p, steps, index - 1, scope),
                _ => Ok(false),
            };
        }
        let mut current = parent;
        while let Some(c) = current {
            if Some(c) == scope {
                break;
            }
            if self.match_from(c, steps, index - 1, scope)? {
                return Ok(true);
            }
            current = self.parent(c);
        }
        Ok(false)
    }

    fn matches_list(&self, id: InstanceId, list: &[Selector], scope: Option<InstanceId>) -> LuaResult<bool> {
        for selector in list {
            if !selector.steps.is_empty() && self.match_from(id, &selector.steps, selector.steps.len() - 1, scope)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn has_match(&self, id: InstanceId, list: &[Selector]) -> LuaResult<bool> {
        let children = self.engine.state.borrow().dom.at(id).children.clone();
        for child in children {
            if self.matches_list(child, list, Some(id))? || self.has_match_under(child, list, id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn has_match_under(&self, node: InstanceId, list: &[Selector], scope: InstanceId) -> LuaResult<bool> {
        let children = self.engine.state.borrow().dom.at(node).children.clone();
        for child in children {
            if self.matches_list(child, list, Some(scope))? || self.has_match_under(child, list, scope)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn collect(&self, node: InstanceId, list: &[Selector], scope: InstanceId, result: &mut Vec<InstanceId>) -> LuaResult<()> {
        let children = self.engine.state.borrow().dom.at(node).children.clone();
        for child in children {
            if self.matches_list(child, list, Some(scope))? {
                result.push(child);
            }
            self.collect(child, list, scope, result)?;
        }
        Ok(())
    }
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let Some(base) = reflection.class_id("Instance") else { return };
    let cache: Rc<RefCell<FxHashMap<Vec<u8>, Rc<Vec<Selector>>>>> = Rc::default();
    let query = {
        let cache = cache.clone();
        move |lua: &Lua, engine: &Rc<Engine>, id: InstanceId, text: &[u8]| -> Result<Vec<InstanceId>, Flow> {
            project::materialize_deep(engine, id);
            let cached = cache.borrow().get(text).cloned();
            let list = match cached {
                Some(list) => list,
                None => {
                    let list = match parse_list(text) {
                        Ok(list) => Rc::new(list),
                        Err(message) => return raise_plain(message),
                    };
                    cache.borrow_mut().insert(text.to_vec(), list.clone());
                    list
                }
            };
            let matcher = Matcher { lua, engine };
            // (each selector of a list answers in turn, in tree order, and an instance two of them match is listed twice)
            let mut result = Vec::new();
            for selector in list.iter() {
                match matcher.collect(id, std::slice::from_ref(selector), id, &mut result) {
                    Ok(()) => {}
                    // (a comparison Studio does not support raises its own text, with no position)
                    Err(LuaError::RuntimeError(message)) if message.starts_with("Property ") => return raise_plain(message),
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(result)
        }
    };
    let query = Rc::new(query);
    if let Some(name) = reflection.name_id("QueryDescendants") {
        let query = query.clone();
        behaviors.add_method(base, name, method(move |lua, engine, id, args| {
            let text = args.string(1).map(|s| s.as_bytes().to_vec()).unwrap_or_default();
            // (the call's own touch of the root says whether it was cold)
            let cold = engine.state.borrow().sched.touched.borrow_mut().take_last_cold();
            let found = query(lua, engine, id, &text)?;
            // (Studio: about 10 ns a descendant looked at and 15 a match, cost.rs; the frame's first query runs its code cold, and a
            // node the frame had not touched is read cold: Path::Query, QUERY_COLD_NODE)
            {
                let state = engine.state.borrow();
                let mut seconds = state.dom.descendants(id).len() as f64 * crate::cost::QUERIED + found.len() as f64 * crate::cost::QUERY_FOUND;
                let mut touched = state.sched.touched.borrow_mut();
                seconds += touched.first_path(crate::cost::Path::Query) + if cold { crate::cost::QUERY_COLD } else { 0.0 };
                drop(touched);
                state.sched.charge(seconds);
                crate::boundary::add_cost(seconds);
            }
            let table = lua.create_table_with_capacity(found.len(), 0)?;
            for id in found {
                table.raw_push(instance_value(lua, engine, id)?)?;
            }
            Ok(values([LuaValue::Table(table)]))
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<Selector> {
        parse_list(text.as_bytes()).unwrap()
    }

    fn error(text: &str) -> String {
        parse_list(text.as_bytes()).err().unwrap()
    }

    /// The grammar and the messages Studio gave (an empty test place, 2026-10-23, a handler_order probe).
    #[test]
    fn parses_selectors_as_studio_does() {
        let list = parse("Frame#Main>.Tagged[Visible = true], TextLabel:not(#Skip)");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].steps.len(), 2);
        assert_eq!(list[0].steps[0].compound.classes, ["Frame"]);
        assert_eq!(list[0].steps[0].compound.names, ["Main"]);
        assert!(list[0].steps[1].child_only);
        assert_eq!(list[0].steps[1].compound.tags, ["Tagged"]);
        assert_eq!(list[0].steps[1].compound.properties, vec![("Visible".to_string(), false, Some("true".to_string()))]);
        assert_eq!(list[1].steps[0].compound.not.len(), 1);
        // spaces never make a step: "Model > Part" is a child step, "Part Part" one step with two classes
        assert!(parse("A > B")[0].steps[1].child_only && parse("A  >  B")[0].steps[1].child_only && !parse("A >> B")[0].steps[1].child_only);
        assert_eq!(parse("Part Part")[0].steps.len(), 1);
        assert_eq!(parse("Model :has(> Part)")[0].steps[0].compound.has.len(), 1);
        assert!(parse("").is_empty() && parse("  ").is_empty());
        for (text, message) in [
            ("*", "Expected at least one filter"),
            (",Part", "Expected at least one filter"),
            ("Part,,Part", "Expected at least one filter"),
            ("Part >", "Expected at least one filter"),
            ("Part >> >> Part", "Expected at least one filter"),
            ("Part,", "Expected at least one combinator or a filter"),
            ("Part:not(", "Expected at least one combinator or a filter"),
            ("Part:not()", "Expected at least one filter"),
            ("#", "Expected identifier for a name"),
            (".", "Expected identifier for a tag name"),
            (":", "Expected identifier for a pseudo-class name"),
            ("Part:bad(1)", "Pseudo-class 'bad' filter is not supported"),
            ("Part[", "Expected identifier for a property name"),
            ("[Anchored]", "comparison operator expected after property name"),
            ("Part[Anchored ~= true]", "comparison operator expected after property name"),
            ("Part[Anchored == false]", "Expected a property value"),
            ("Part[Name='P']", "Expected a property value"),
            ("Part[Anchored = false", "Expected ']' to complete a property filter"),
            ("Part)", "Unexpected trailing character: ')'"),
        ] {
            assert_eq!(error(text), message, "{text}");
        }
    }
}
