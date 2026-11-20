//! Derived properties (port of `runtime/engine/derived.luau` and instance.luau Derive): views of
//! another stored property with Studio's conversions and change-event behaviour. Font -> FontFace,
//! FontSize -> TextSize, TextWrap -> TextWrapped, TextColor / BackgroundColor / BorderColor ->
//! *Color3, Transparency -> BackgroundTransparency (and TextTransparency on text objects),
//! IgnoreGuiInset -> ScreenInsets, Localize -> AutoLocalize, *Content -> string asset ids.
//!
//! A view's getter reads the source and converts; its setter coerces, converts back and stores the
//! source (through RawSet, so the source's own events fire). Measured: a view's change events fire
//! only when its value changes, some before and some after the source's events, and some views
//! (Transparency, TextWrap, Localize) fire none.

use std::{cell::RefCell, rc::Rc};

use rbx_dom_weak::types::{self as dom, Variant};
use rustc_hash::FxHashMap;

use super::{
    ClassId, Engine, Hooks, InstanceId, NameId, Reflection, State,
    instance as core,
    lua::{convert, raise},
    reflection::ScriptType,
    services::{GetterFn, SetterFn},
    value::{ContentValue, Str, Value},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewEvents {
    /// The view's events fire before the source's
    Before,
    After,
    None,
}

pub type ReadFn = Rc<dyn Fn(Option<&Value>) -> Value>;
pub type WriteFn = Rc<dyn Fn(&mut State, &Reflection, &Hooks, InstanceId, Value)>;

pub struct DerivedView {
    pub name: NameId,
    pub source: NameId,
    pub events: ViewEvents,
    pub read: ReadFn,
    pub write: WriteFn,
}

/// Views by the class that declares them, and a per-(class, source) cache of the views an
/// instance's source change must consider (DerivedFor).
#[derive(Default)]
pub struct DerivedViews {
    by_class: FxHashMap<(ClassId, NameId), Vec<Rc<DerivedView>>>,
    cache: RefCell<FxHashMap<(ClassId, NameId), Rc<[Rc<DerivedView>]>>>,
    /// BasePart, and the events a part fires beside a change of each source ([`Companions`])
    part: Option<ClassId>,
    part_companions: FxHashMap<NameId, Rc<Companions>>,
}

/// The Changed events (and property signals) a part fires beside a change of one of its properties, each whether or not
/// its own value changed, in this order around the source's own: measured in a playing client (Studio 0.744,
/// tools/parity/probes/derived_changed.luau), for a part outside the DataModel, in the Workspace, anchored or not, a
/// MeshPart and a WedgePart alike (a part welded to it hears nothing):
/// - CFrame (any write that moves the part: Position, Orientation, Rotation, PivotTo): then Position,
///   AssemblyCenterOfMass, Orientation and Rotation, even when only the position or only the rotation changed;
/// - Size: then AssemblyMass and Mass, even at the same volume;
/// - Color: BrickColor first, even when the nearest palette colour stays the same (a BrickColor write changes Color);
/// - Material: then CurrentPhysicalProperties, even at the same density;
/// - CustomPhysicalProperties: CurrentPhysicalProperties first, then AssemblyMass and AssemblyCenterOfMass;
/// - Massless: then AssemblyMass; Anchored: then AssemblyMass and AssemblyCenterOfMass.
pub struct Companions {
    pub before: Vec<NameId>,
    pub after: Vec<NameId>,
}

impl Companions {
    fn lists(&self, name: NameId) -> bool {
        self.before.contains(&name) || self.after.contains(&name)
    }
}

impl DerivedViews {
    fn add(&mut self, class: ClassId, view: DerivedView) {
        self.by_class.entry((class, view.source)).or_default().push(Rc::new(view));
        self.cache.borrow_mut().clear();
    }

    /// DerivedFor: the views of `source` on `class` and its superclasses (own class first).
    pub fn views_for(&self, reflection: &Reflection, class: ClassId, source: NameId) -> Rc<[Rc<DerivedView>]> {
        if let Some(found) = self.cache.borrow().get(&(class, source)) {
            return found.clone();
        }
        let mut found = Vec::new();
        let mut current = Some(class);
        while let Some(id) = current {
            if let Some(views) = self.by_class.get(&(id, source)) {
                found.extend(views.iter().cloned());
            }
            current = reflection.class(id).superclass;
        }
        let found: Rc<[Rc<DerivedView>]> = found.into();
        self.cache.borrow_mut().insert((class, source), found.clone());
        found
    }

    pub fn is_empty(&self) -> bool {
        self.by_class.is_empty()
    }

    /// The events an instance of `class` fires beside a change of `source`, if it is a part with some.
    pub fn companions_for(&self, reflection: &Reflection, class: ClassId, source: NameId) -> Option<Rc<Companions>> {
        if self.part_companions.is_empty() || !self.part.is_some_and(|part| reflection.class(class).is_a(part)) {
            return None;
        }
        self.part_companions.get(&source).cloned()
    }

    fn add_part_companions(&mut self, reflection: &Reflection) {
        let id = |name: &str| reflection.name_id(name);
        let names = |list: &[&str]| list.iter().filter_map(|name| id(name)).collect::<Vec<_>>();
        self.part = reflection.class_id("BasePart");
        for (source, before, after) in [
            ("CFrame", &[][..], &["Position", "AssemblyCenterOfMass", "Orientation", "Rotation"][..]),
            ("Size", &[], &["AssemblyMass", "Mass"]),
            ("Color", &["BrickColor"], &[]),
            ("Material", &[], &["CurrentPhysicalProperties"]),
            ("CustomPhysicalProperties", &["CurrentPhysicalProperties"], &["AssemblyMass", "AssemblyCenterOfMass"]),
            ("Massless", &[], &["AssemblyMass"]),
            ("Anchored", &[], &["AssemblyMass", "AssemblyCenterOfMass"]),
        ] {
            if let Some(source) = id(source) {
                self.part_companions.insert(source, Rc::new(Companions { before: names(before), after: names(after) }));
            }
        }
    }
}

fn nearest_brick_color(value: Option<&Value>) -> Value {
    let Some(color) = value.and_then(Value::as_color3) else {
        return Value::Dom(Rc::new(Variant::BrickColor(dom::BrickColor::from_number(194).unwrap_or(dom::BrickColor::MediumStoneGrey))));
    };
    let mut best: Option<(f64, dom::BrickColor)> = None;
    for number in 1..=1032u16 {
        let Some(candidate) = dom::BrickColor::from_number(number) else { continue };
        let c = candidate.to_color3uint8();
        let (r, g, b) = ((c.r as f32 / 255.0) as f64, (c.g as f32 / 255.0) as f64, (c.b as f32 / 255.0) as f64);
        let distance = (r - color.r as f64).powi(2) + (g - color.g as f64).powi(2) + (b - color.b as f64).powi(2);
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    Value::Dom(Rc::new(Variant::BrickColor(best.map(|(_, b)| b).unwrap_or(dom::BrickColor::MediumStoneGrey))))
}

fn brick_color3(value: &Value) -> Option<dom::Color3> {
    match value {
        Value::Dom(variant) => match &**variant {
            Variant::BrickColor(brick) => {
                let c = brick.to_color3uint8();
                Some(dom::Color3::new(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0))
            }
            _ => None,
        },
        _ => None,
    }
}

fn raw_set(state: &mut State, reflection: &Reflection, hooks: &Hooks, id: InstanceId, name: NameId, value: Value) {
    core::raw_set(state, reflection, hooks, id, name, value);
}

/// Derived.Install: registers every view and its getter / setter behaviours.
pub fn install(engine: &Rc<Engine>) {
    let reflection = engine.reflection.clone();
    let mut views = DerivedViews::default();
    let mut getters: Vec<(ClassId, NameId, GetterFn)> = Vec::new();
    let mut setters: Vec<(ClassId, NameId, SetterFn)> = Vec::new();

    let mut derive = |views: &mut DerivedViews, class_name: &str, name: &str, source: &str, events: ViewEvents, read: ReadFn, write: WriteFn| {
        let (Some(class), Some(name), Some(source)) = (reflection.class_id(class_name), reflection.name_id(name), reflection.name_id(source)) else {
            return;
        };
        let (getter, setter) = view_behaviours(name, source, read.clone(), write.clone());
        getters.push((class, name, getter));
        setters.push((class, name, setter));
        views.add(class, DerivedView { name, source, events, read, write });
    };

    let name_of = |n: &str| reflection.name_id(n);
    let unknown_font = reflection.enum_item("Font", "Unknown");
    let mut font_sizes: Vec<(f64, u32)> = Vec::new();
    if let Some(id) = reflection.enum_id("FontSize") {
        for &item in &reflection.enums[id as usize].items {
            let name = &reflection.enum_item_info(item).name;
            let digits: String = name.chars().filter(|c| c.is_ascii_digit()).collect();
            if let Ok(size) = digits.parse::<f64>() {
                font_sizes.push((size, item));
            }
        }
    }
    font_sizes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let font_sizes = Rc::new(font_sizes);

    for class_name in ["TextLabel", "TextButton", "TextBox"] {
        // Measured: setting Font fires FontFace then Font; Enum.Font.Unknown is ignored.
        let e = engine.clone();
        let read: ReadFn = Rc::new(move |face| match face {
            Some(Value::Font(face)) => super::lua::types::font_to_enum(&e, face).map_or(Value::Nil, Value::EnumItem),
            _ => unknown_font.map_or(Value::Nil, Value::EnumItem),
        });
        let e = engine.clone();
        let font_face = name_of("FontFace");
        let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
            let Some(item) = value.as_enum() else { return };
            if Some(item) == unknown_font {
                return;
            }
            let face = super::lua::types::font_from_enum(&e, item);
            if let Some(font_face) = font_face {
                raw_set(state, reflection, hooks, id, font_face, Value::Font(Rc::new(face)));
            }
        });
        derive(&mut views, class_name, "Font", "FontFace", ViewEvents::After, read, write);

        // Measured: FontSize reads as the smallest Enum.FontSize >= TextSize (Size96 at most).
        let sizes = font_sizes.clone();
        let read: ReadFn = Rc::new(move |size| {
            let value = size.and_then(Value::as_number).unwrap_or(8.0);
            sizes.iter().find(|(s, _)| *s >= value).or(sizes.last()).map_or(Value::Nil, |(_, item)| Value::EnumItem(*item))
        });
        let text_size = name_of("TextSize");
        let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
            let Some(item) = value.as_enum() else { return };
            let digits: String = reflection.enum_item_info(item).name.chars().filter(|c| c.is_ascii_digit()).collect();
            if let (Ok(size), Some(text_size)) = (digits.parse::<f64>(), text_size) {
                raw_set(state, reflection, hooks, id, text_size, Value::Number(size));
            }
        });
        derive(&mut views, class_name, "FontSize", "TextSize", ViewEvents::After, read, write);

        let identity: ReadFn = Rc::new(|value| value.cloned().unwrap_or(Value::Nil));
        let wrapped = name_of("TextWrapped");
        let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
            if let Some(wrapped) = wrapped {
                raw_set(state, reflection, hooks, id, wrapped, value);
            }
        });
        derive(&mut views, class_name, "TextWrap", "TextWrapped", ViewEvents::None, identity, write);
        color_view(&mut derive_wrapper(&mut views, &mut derive), class_name, "TextColor", "TextColor3", &reflection);
    }
    color_view(&mut derive_wrapper(&mut views, &mut derive), "GuiObject", "BackgroundColor", "BackgroundColor3", &reflection);
    color_view(&mut derive_wrapper(&mut views, &mut derive), "GuiObject", "BorderColor", "BorderColor3", &reflection);
    // A part's BrickColor is the nearest palette colour to its Color, and writing it sets Color (measured in a playing
    // client, Studio 0.744: either write fires BrickColor's events and then Color's, tools/parity/probes/changed.luau and
    // derived_changed.luau; ree kept the two apart, so a BrickColor write never reached the part's colour)
    color_view(&mut derive_wrapper(&mut views, &mut derive), "BasePart", "BrickColor", "Color", &reflection);

    // Measured: Transparency reads BackgroundTransparency; on text objects writing it also sets
    // TextTransparency (events: TextTransparency, BackgroundTransparency; none for Transparency).
    let identity: ReadFn = Rc::new(|value| value.cloned().unwrap_or(Value::Nil));
    let text_transparency = name_of("TextTransparency");
    let background = name_of("BackgroundTransparency");
    let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
        let class = state.dom.at(id).class;
        let text = ["TextLabel", "TextButton", "TextBox"].iter().any(|c| reflection.is_a(class, c));
        if let (true, Some(name)) = (text, text_transparency) {
            raw_set(state, reflection, hooks, id, name, value.clone());
        }
        if let Some(name) = background {
            raw_set(state, reflection, hooks, id, name, value);
        }
    });
    derive(&mut views, "GuiObject", "Transparency", "BackgroundTransparency", ViewEvents::None, identity, write);

    // Measured: IgnoreGuiInset is true unless ScreenInsets is CoreUISafeInsets; setting it true
    // selects DeviceSafeInsets (events: IgnoreGuiInset, ScreenInsets).
    let core_insets = reflection.enum_item("ScreenInsets", "CoreUISafeInsets");
    let device_insets = reflection.enum_item("ScreenInsets", "DeviceSafeInsets");
    let read: ReadFn = Rc::new(move |insets| Value::Bool(insets.and_then(Value::as_enum) != core_insets));
    let screen_insets = name_of("ScreenInsets");
    let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
        let on = !matches!(value, Value::Nil | Value::Bool(false));
        let item = if on { device_insets } else { core_insets };
        if let (Some(name), Some(item)) = (screen_insets, item) {
            raw_set(state, reflection, hooks, id, name, Value::EnumItem(item));
        }
    });
    derive(&mut views, "ScreenGui", "IgnoreGuiInset", "ScreenInsets", ViewEvents::Before, read, write);

    let identity: ReadFn = Rc::new(|value| value.cloned().unwrap_or(Value::Nil));
    let auto_localize = name_of("AutoLocalize");
    let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
        if let Some(name) = auto_localize {
            raw_set(state, reflection, hooks, id, name, value);
        }
    });
    derive(&mut views, "GuiBase2d", "Localize", "AutoLocalize", ViewEvents::None, identity, write);

    // Measured (Studio 2026-10-16): Model.WorldPivot is the stored WorldPivotData (what files and
    // clones carry), identity while unset; Model:GetPivot reads the same store (lua/services.luau).
    let read: ReadFn = Rc::new(|value| match value {
        Some(Value::Dom(variant)) if matches!(**variant, Variant::CFrame(_)) => Value::Dom(variant.clone()),
        Some(Value::Dom(variant)) => match &**variant {
            Variant::OptionalCFrame(Some(cframe)) => Value::Dom(Rc::new(Variant::CFrame(*cframe))),
            _ => Value::Dom(Rc::new(Variant::CFrame(dom::CFrame::new(dom::Vector3::new(0.0, 0.0, 0.0), dom::Matrix3::identity())))),
        },
        _ => Value::Dom(Rc::new(Variant::CFrame(dom::CFrame::new(dom::Vector3::new(0.0, 0.0, 0.0), dom::Matrix3::identity())))),
    });
    let world_pivot_data = name_of("WorldPivotData");
    let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
        if let Some(name) = world_pivot_data {
            raw_set(state, reflection, hooks, id, name, value);
        }
    });
    derive(&mut views, "Model", "WorldPivot", "WorldPivotData", ViewEvents::None, read, write);

    // Content views of string asset ids: every Content property with a string sibling
    // (Reflection::content_sibling: Image, AnimationId, TextureID, Sound's SoundId). Measured: events
    // ImageContent, Image; each readable and settable through the other (Animation, SpecialMesh,
    // Sound, MeshPart, Sky, 2026-10-12). Which of the two a file holds does not matter here: Studio 0.740's
    // dump marked the string as the serialized one, 0.747's marks the Content, and the pairs are the same.
    let mut content_views: Vec<(String, String, String)> = Vec::new();
    for class in &reflection.classes {
        for (&property_name, &index) in &class.properties {
            let property = reflection.property(index);
            if property.owner != class.id || property.script_type != ScriptType::Content {
                continue;
            }
            let Some(source) = reflection.content_sibling(class.id, property_name) else { continue };
            content_views.push((class.name.to_string(), reflection.name(property_name).to_string(), reflection.name(source).to_string()));
        }
    }
    content_views.sort();
    for (class_name, name, source_name) in content_views {
        let read: ReadFn = Rc::new(|uri| match uri {
            Some(Value::String(uri)) if !uri.is_empty() => Value::Content(Rc::new(ContentValue::Uri(uri.clone()))),
            _ => Value::Content(Rc::new(ContentValue::None)),
        });
        let source = name_of(&source_name);
        let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
            let uri = match &value {
                Value::Content(content) => content.uri().cloned().unwrap_or_default(),
                _ => Str::default(),
            };
            if let Some(source) = source {
                raw_set(state, reflection, hooks, id, source, Value::String(uri));
            }
        });
        derive(&mut views, &class_name, &name, &source_name, ViewEvents::Before, read, write);
    }

    let mut behaviors = engine.behaviors.borrow_mut();
    for (class, name, getter) in getters {
        behaviors.add_getter(class, name, getter);
    }
    for (class, name, setter) in setters {
        behaviors.add_setter(class, name, setter);
    }
    drop(behaviors);
    views.add_part_companions(&reflection);
    engine.hooks.borrow_mut().derived = views;
    engine.clear_member_cache();
}

/// A view's getter (reads its source and converts) and setter (coerces to the view's type, then
/// writes the source through `write`).
fn view_behaviours(name: NameId, source: NameId, read: ReadFn, write: WriteFn) -> (GetterFn, SetterFn) {
    let getter: GetterFn = Rc::new(move |lua, engine, id| {
        let value = (read)(engine.state.borrow().dom.raw_get(id, source));
        Ok(convert::to_lua(lua, engine, &value)?)
    });
    let setter: SetterFn = Rc::new(move |lua, engine, id, value| {
        let instance_class = engine.state.borrow().dom.at(id).class;
        let info = engine.reflection.class_property(instance_class, name).cloned();
        if info.as_ref().is_some_and(|i| i.read_only) {
            return raise(format!("Unable to assign property {}. Property is read only", engine.reflection.name(name)));
        }
        let coerced = match &info {
            Some(info) => convert::coerce(lua, engine, info, &value)?,
            None => convert::from_lua(&value),
        };
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        (write)(&mut state, &engine.reflection, &hooks, id, coerced);
        Ok(())
    });
    (getter, setter)
}

/// Registers a view after Derived.Install (engine::world's views of a part's or attachment's
/// CFrame): its getter / setter behaviours and its change events. False when the class or a name
/// is unknown.
pub fn add_view(engine: &Rc<Engine>, class_name: &str, name: &str, source: &str, events: ViewEvents, read: ReadFn, write: WriteFn) -> bool {
    let reflection = &engine.reflection;
    let (Some(class), Some(name), Some(source)) = (reflection.class_id(class_name), reflection.name_id(name), reflection.name_id(source)) else {
        return false;
    };
    let (getter, setter) = view_behaviours(name, source, read.clone(), write.clone());
    {
        let mut behaviors = engine.behaviors.borrow_mut();
        behaviors.add_getter(class, name, getter);
        behaviors.add_setter(class, name, setter);
    }
    engine.hooks.borrow_mut().derived.add(class, DerivedView { name, source, events, read, write });
    engine.clear_member_cache();
    true
}

type DeriveFn<'a> = dyn FnMut(&mut DerivedViews, &str, &str, &str, ViewEvents, ReadFn, WriteFn) + 'a;

struct DeriveWrapper<'a, 'b> {
    views: &'a mut DerivedViews,
    derive: &'a mut DeriveFn<'b>,
}

fn derive_wrapper<'a, 'b>(views: &'a mut DerivedViews, derive: &'a mut DeriveFn<'b>) -> DeriveWrapper<'a, 'b> {
    DeriveWrapper { views, derive }
}

fn color_view(wrapper: &mut DeriveWrapper, class_name: &str, name: &str, source: &str, reflection: &Reflection) {
    let read: ReadFn = Rc::new(nearest_brick_color);
    let source_id = reflection.name_id(source);
    let write: WriteFn = Rc::new(move |state, reflection, hooks, id, value| {
        if let (Some(color), Some(source)) = (brick_color3(&value), source_id) {
            raw_set(state, reflection, hooks, id, source, Value::Color3(color));
        }
    });
    (wrapper.derive)(wrapper.views, class_name, name, source, ViewEvents::Before, read, write);
}

/// NotifyChanged's view pass: which views of `source` fire (those someone listens to whose value
/// changed), split into those before and after the source's own events. A view a part's companion events list
/// (`companions`) fires there instead, whatever its value.
pub fn changed_views(
    state: &State,
    reflection: &Reflection,
    hooks: &Hooks,
    id: InstanceId,
    source: NameId,
    old: Option<&Value>,
    companions: Option<&Companions>,
) -> (Vec<NameId>, Vec<NameId>) {
    if hooks.derived.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let instance = state.dom.at(id);
    let views = hooks.derived.views_for(reflection, instance.class, source);
    if views.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let signals = instance.signals.as_deref();
    // Only signals with handlers can observe a view's event (firing one without handlers does
    // nothing), so views nobody listens to are not read (a BrickColor view is a palette search)
    let handled = |signal: Option<super::signal::SignalId>| signal.is_some_and(|signal| state.signals.has_handlers(signal));
    let changed_signal = handled(signals.and_then(|s| s.event(super::K::Changed as NameId)));
    let new = state.dom.raw_get(id, source);
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for view in views.iter() {
        if companions.is_some_and(|companions| companions.lists(view.name)) {
            continue;
        }
        let listening = view.events != ViewEvents::None
            && (changed_signal || handled(signals.and_then(|s| s.property(view.name))));
        let mut fires = listening;
        if listening {
            if let Some(old) = old {
                let (a, b) = ((view.read)(Some(old).filter(|v| !v.is_nil())), (view.read)(new));
                fires = !a.same(&b);
            }
        }
        if fires {
            match view.events {
                ViewEvents::Before => before.push(view.name),
                ViewEvents::After => after.push(view.name),
                ViewEvents::None => {}
            }
        }
    }
    (before, after)
}
