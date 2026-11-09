//! Recorder checks: a scripted world, run frame by frame, recorded into a scene, a playback, the
//! events and the camera track.

use mlua::prelude::*;
use rbx_dom_weak::types::Variant;
use serde_json::{Map, Value as Json};

use super::Recorder;
use crate::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
    scheduler,
};

/// Recordings' temporary folders take a number from this as well as the process and the clock: two tests started in one
/// tick of Windows' clock (100 ns) took the same folder and wrote each other's files into it
static TEST_DIRS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs `source` in a play-mode engine for `frames` frames at 60 a second, recording; returns the
/// output directory's files as read back.
fn record(source: &str, frames: u32) -> (Json, Json, rbx_dom_weak::WeakDom, Json) {
    let (playback, events, scene, summary, _) = record_with(source, frames, false);
    (playback, events, scene, summary)
}

/// [`record`], also returning scene.rbxm's bytes; `walk_always`: the recorder walks the world every
/// frame instead of only after something was parented.
fn record_with(source: &str, frames: u32, walk_always: bool) -> (Json, Json, rbx_dom_weak::WeakDom, Json, Vec<u8>) {
    record_in(source, frames, walk_always, &[])
}

/// [`record_with`] in a place whose Lighting holds `own` (class, name) before the run.
fn record_in(source: &str, frames: u32, walk_always: bool, own: &[(&str, &str)]) -> (Json, Json, rbx_dom_weak::WeakDom, Json, Vec<u8>) {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    if !own.is_empty() {
        let lighting = crate::services::find_service(&engine, engine.world().game, "Lighting").expect("Lighting");
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        for (class, name) in own {
            let class = engine.reflection.class_id(class).unwrap();
            let id = crate::instance::create_named(&mut state, &engine.reflection, class, name);
            crate::instance::set_parent(&mut state, &engine.reflection, &hooks, id, Some(lighting)).expect("parent");
        }
    }
    let recorder = Recorder::install(&engine, 60.0);
    if walk_always {
        recorder.walk_every_frame();
    }
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Harness")
    };
    let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
    let function = lua.load(source).set_name("=Harness").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("harness failed: {message}");
    }
    recorder.sample(&engine, 0);
    for frame in 1..=frames {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("frame");
        recorder.sample(&engine, frame);
    }
    let unique = TEST_DIRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("rur-record-test-{}-{}-{unique}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let summary = recorder.write(&engine, &out, Map::new()).expect("write");
    let read = |name: &str| -> Json { serde_json::from_str(&std::fs::read_to_string(out.join(name)).unwrap()).unwrap() };
    let playback = read("playback.json");
    let events = read("events.json");
    let bytes = std::fs::read(out.join("scene.rbxm")).unwrap();
    let scene = rbx_binary::from_reader(bytes.as_slice()).expect("scene.rbxm");
    let _ = std::fs::remove_dir_all(&out);
    drop(lua);
    engine.teardown();
    (playback, events, scene, summary, bytes)
}

fn find<'a>(list: &'a Json, test: impl Fn(&Json) -> bool) -> Vec<&'a Json> {
    list.as_array().map(|items| items.iter().filter(|item| test(item)).collect()).unwrap_or_default()
}

fn instance<'a>(dom: &'a rbx_dom_weak::WeakDom, path: &[&str]) -> &'a rbx_dom_weak::Instance {
    let mut current = dom.root();
    for name in path {
        let child = current.children().iter().map(|r| dom.get_by_ref(*r).unwrap()).find(|c| c.name == *name);
        current = child.unwrap_or_else(|| panic!("no {name} in {path:?}"));
    }
    current
}

#[test]
fn a_scripted_world_plays_back() {
    let (playback, events, scene, summary) = record(
        r#"
        local TweenService = game:GetService("TweenService")
        local Folder = Instance.new("Folder")
        Folder.Name = "Effect"
        Folder.Parent = workspace
        -- in the world from the start, then moved by a tween
        local Mover = Instance.new("Part")
        Mover.Name = "Mover"
        Mover.Anchored = true
        Mover.Size = Vector3.new(2, 2, 2)
        Mover.CFrame = CFrame.new(0, 0, 0)
        Mover.Parent = Folder
        local Emitter = Instance.new("ParticleEmitter")
        Emitter.Rate = 0
        Emitter.Parent = Mover
        TweenService:Create(Mover, TweenInfo.new(0.25, Enum.EasingStyle.Linear), {CFrame = CFrame.new(0, 10, 0)}):Play()
        -- two siblings of one name
        for _ = 1, 2 do
            local Twin = Instance.new("Part")
            Twin.Name = "Twin"
            Twin.Parent = Folder
        end
        -- in the world from the start until Debris takes it
        local Shard = Instance.new("Part")
        Shard.Name = "Shard"
        Shard.Parent = Folder
        game:GetService("Debris"):AddItem(Shard, 0.3)
        task.delay(0.1, function()
            Emitter:Emit(12)
            print("emitted")
        end)
        -- appears later, with a ground decal on its face, then leaves
        task.delay(0.2, function()
            local Plate = Instance.new("Part")
            Plate.Name = "Plate"
            Plate.Transparency = 1
            Plate.Size = Vector3.new(8, 0.1, 8)
            local Gui = Instance.new("SurfaceGui")
            Gui.Face = Enum.NormalId.Top
            local Image = Instance.new("ImageLabel")
            Image.Size = UDim2.fromScale(0.5, 0.5)
            Image.Image = "rbxassetid://1"
            Image.Parent = Gui
            Gui.Parent = Plate
            local Corner = Instance.new("UICorner")
            Corner.CornerRadius = UDim.new(1, 0)
            Corner.Parent = Image
            Plate.Parent = Folder
            TweenService:Create(Image, TweenInfo.new(0.1, Enum.EasingStyle.Linear), {ImageTransparency = 1, Size = UDim2.fromScale(1, 1)}):Play()
            task.delay(0.2, function()
                Plate:Destroy()
            end)
        end)
        workspace.CurrentCamera.CFrame = CFrame.new(0, 5, 20)
        "#,
        36,
    );
    // the scene: every instance as it appeared, siblings of one name told apart
    let mover = instance(&scene, &["Effect", "Mover"]);
    assert!(matches!(mover.properties.get(&"CFrame".into()), Some(Variant::CFrame(c)) if c.position.y == 0.0));
    instance(&scene, &["Effect", "Twin"]);
    instance(&scene, &["Effect", "Twin~2"]);
    let plate = instance(&scene, &["Effect", "Plate"]);
    assert_eq!(plate.properties.get(&"Transparency".into()), Some(&Variant::Float32(1.0)), "hidden until it appears");
    let gui = instance(&scene, &["Effect", "Plate", "SurfaceGui"]);
    assert_eq!(gui.properties.get(&"Enabled".into()), Some(&Variant::Bool(false)), "hidden until it appears");
    let corner = instance(&scene, &["Effect", "Plate", "SurfaceGui", "ImageLabel", "UICorner"]);
    assert_eq!(corner.properties.get(&"CornerRadius".into()), Some(&Variant::UDim(rbx_dom_weak::types::UDim::new(1.0, 0))), "the view's value");
    assert!(scene.root().children().iter().all(|r| scene.get_by_ref(*r).unwrap().class != "Camera"), "the camera is not in the scene");

    // motion of the tweened part: keys from 0 to 0.25 s, 10 studs up at the end
    let motion = find(&playback["motion"], |m| m["target"] == "Effect.Mover");
    assert_eq!(motion.len(), 1, "{}", playback["motion"]);
    let keys = motion[0]["keys"].as_array().unwrap();
    let last = keys.last().unwrap();
    assert!((last["at"].as_f64().unwrap() - 0.25).abs() < 0.02 && (last["cframe"][1].as_f64().unwrap() - 10.0).abs() < 1e-3, "{last}");
    // the emit at its frame (a task.delay resumes at the first frame at or after it)
    let emits = find(&playback["actions"], |a| a["emit"].is_number());
    assert_eq!(emits.len(), 1);
    assert_eq!(emits[0]["target"], "Effect.Mover.ParticleEmitter");
    assert_eq!(emits[0]["emit"], 12);
    assert!((emits[0]["at"].as_f64().unwrap() - 0.1).abs() < 0.02);
    // the plate shows at 0.2 s and hides when destroyed, the GUI with it
    let plate_sets = find(&playback["actions"], |a| a["target"] == "Effect.Plate");
    assert!(plate_sets.is_empty(), "a part at Transparency 1 has nothing to show: {plate_sets:?}");
    let gui_sets = find(&playback["actions"], |a| a["target"] == "Effect.Plate.SurfaceGui");
    let enabled: Vec<(f64, bool)> = gui_sets.iter().map(|a| (a["at"].as_f64().unwrap(), a["set"]["Enabled"].as_bool().unwrap())).collect();
    assert_eq!(enabled.len(), 2, "{gui_sets:?}");
    assert!(enabled[0].1 && (enabled[0].0 - 0.2).abs() < 0.02 && !enabled[1].1 && (enabled[1].0 - 0.4).abs() < 0.03, "{enabled:?}");
    // the part Debris removes hides when it goes
    let shard: Vec<(f64, f64)> = find(&playback["actions"], |a| a["target"] == "Effect.Shard").iter().map(|a| (a["at"].as_f64().unwrap(), a["set"]["Transparency"].as_f64().unwrap())).collect();
    assert!(shard.len() == 1 && (shard[0].0 - 0.3).abs() < 0.03 && shard[0].1 == 1.0, "{shard:?}");
    // the image's tweens: UDim2 sizes and transparency keyed while they change
    let tweens = find(&playback["tweens"], |t| t["target"] == "Effect.Plate.SurfaceGui.ImageLabel");
    let size = tweens.iter().find(|t| t["property"] == "Size").expect("a Size tween");
    let size_keys = size["keys"].as_array().unwrap();
    // (the tween's first step lands in the frame the plate appears in, as in Roblox: the file holds it)
    let first = size_keys.first().unwrap()["value"][0].as_f64().unwrap();
    assert!(first > 0.5 && first < 0.7, "{size}");
    assert_eq!(size_keys.last().unwrap()["value"], serde_json::json!([1.0, 0, 1.0, 0]));
    assert!(tweens.iter().any(|t| t["property"] == "ImageTransparency"));
    // events and summary
    let output = find(&events["output"], |o| o["message"] == "emitted");
    assert_eq!(output.len(), 1);
    assert_eq!(summary["frames"], 37);
    assert!(summary["unsupported"].as_array().unwrap().is_empty(), "{summary}");
}

#[test]
fn lighting_additions_play_in_the_lighting() {
    let (playback, _, scene, _) = record(
        r#"
        local Lighting = game:GetService("Lighting")
        task.delay(0.1, function()
            local Flash = Instance.new("ColorCorrectionEffect")
            Flash.Name = "Flash"
            Flash.Brightness = 0.5
            Flash.Parent = Lighting
            Lighting.ExposureCompensation = 2
            task.delay(0.1, function()
                Flash:Destroy()
                Lighting.ExposureCompensation = 0
            end)
        end)
        "#,
        20,
    );
    instance(&scene, &["~Lighting", "Flash"]);
    let parents = find(&playback["actions"], |a| a["target"] == "~Lighting.Flash" && a["parent"].is_string());
    let moves: Vec<&str> = parents.iter().map(|a| a["parent"].as_str().unwrap()).collect();
    assert_eq!(moves, ["Lighting", "~Lighting"]);
    let exposure = find(&playback["actions"], |a| a["target"] == "Lighting" && a["set"]["ExposureCompensation"].is_number());
    let values: Vec<f64> = exposure.iter().map(|a| a["set"]["ExposureCompensation"].as_f64().unwrap()).collect();
    assert_eq!(values, [2.0, 0.0]);
}

/// The engine's Lighting before the run stands in for the place's, which the renderer's stage draws, so
/// every write of the run plays: what the harness writes before frame 0 (its setup) plays from frame 0,
/// a write of the value the engine held plays too, and a tween holds
/// the stage's value (`{"initial": true}`) until the run first changes the property. GlobalShadows and
/// ShadowSoftness play as the other Lighting properties do.
#[test]
fn lighting_writes_before_the_first_frame_play_from_it() {
    let (playback, _, _, summary) = record(
        r#"
        local Lighting = game:GetService("Lighting")
        local Held = Lighting.GlobalShadows
        Lighting.GlobalShadows = Held
        Lighting.Brightness = 0.5
        Lighting.ShadowSoftness = 0.6
        task.delay(0.1, function()
            Lighting.GlobalShadows = not Held
            Lighting.ShadowSoftness = 1
            Lighting.Brightness = 0.5
            game:GetService("TweenService"):Create(Lighting, TweenInfo.new(0.05, Enum.EasingStyle.Linear), {ExposureCompensation = 1}):Play()
        end)
        "#,
        16,
    );
    let sets = |property: &str| -> Vec<(f64, Json)> {
        find(&playback["actions"], |a| a["target"] == "Lighting" && !a["set"][property].is_null()).iter().map(|a| (a["at"].as_f64().unwrap(), a["set"][property].clone())).collect()
    };
    let shadows = sets("GlobalShadows");
    assert_eq!(shadows.len(), 2, "the held value written back, then its opposite: {shadows:?}");
    assert!(shadows[0].1.is_boolean() && shadows[1].1.is_boolean() && shadows[0].1 != shadows[1].1, "{shadows:?}");
    assert!(shadows[0].0 == 0.0 && shadows[1].0 > 0.0, "{shadows:?}");
    let brightness = sets("Brightness");
    assert_eq!(brightness.len(), 1, "a write of the value the run last wrote plays nothing: {brightness:?}");
    assert_eq!((brightness[0].0, brightness[0].1.as_f64().unwrap()), (0.0, 0.5));
    let softness: Vec<(f64, f64)> = sets("ShadowSoftness").iter().map(|(at, value)| (*at, value.as_f64().unwrap())).collect();
    assert_eq!(softness.len(), 2, "{softness:?}");
    assert_eq!(softness[0].0, 0.0);
    assert!((softness[0].1 - 0.6).abs() < 1e-5 && (softness[1].1 - 1.0).abs() < 1e-5, "{softness:?}");
    let tweens = find(&playback["tweens"], |t| t["target"] == "Lighting");
    assert_eq!(tweens.len(), 1, "{tweens:?}");
    let keys = tweens[0]["keys"].as_array().unwrap();
    assert_eq!(tweens[0]["property"], "ExposureCompensation");
    assert_eq!(keys[0]["value"], serde_json::json!({ "initial": true }), "{keys:?}");
    assert!(keys[0]["at"].as_f64().unwrap() > 0.0, "{keys:?}");
    assert_eq!(keys.last().unwrap()["value"].as_f64(), Some(1.0), "{keys:?}");
    assert!(summary["unsupported"].as_array().unwrap().is_empty(), "{summary}");
}

/// The Terrain's Clouds play on `Workspace.Terrain.Clouds` as Lighting's own properties play on
/// `Lighting`: the place's (a Studio-only placeholder, at the engine's defaults) stand in for the clouds
/// the renderer's stage draws, so every write of the run plays, its setup's at frame 0, a write of the
/// value the engine held too, and a tween holds the stage's value until it first changes the property.
/// A Clouds is never recorded as an instance, and one outside the Terrain draws nothing.
#[test]
fn terrain_cloud_writes_play_on_the_stages_clouds() {
    let run = |source: &str, place_clouds: bool| -> (Json, rbx_dom_weak::WeakDom, Json) {
        let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
        let lua = Lua::new();
        engine_lua::start(&lua, &engine).expect("start");
        if place_clouds {
            let terrain = crate::world::terrain(&engine, engine.world().workspace).expect("Terrain");
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("Clouds").unwrap();
            let id = crate::instance::create_named(&mut state, &engine.reflection, class, "Clouds");
            state.dom.at_mut(id).set_flag(crate::dom::flags::STUDIO_ONLY, true);
            state.dom.attach(id, terrain);
        }
        let recorder = Recorder::install(&engine, 60.0);
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            crate::instance::create_named(&mut state, &engine.reflection, class, "Harness")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let function = lua.load(source).set_name("=Harness").set_environment(env).into_function().expect("compile");
        let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
            panic!("harness failed: {message}");
        }
        recorder.sample(&engine, 0);
        for frame in 1..=16 {
            scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("frame");
            recorder.sample(&engine, frame);
        }
        let out = std::env::temp_dir().join(format!("rur-record-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let summary = recorder.write(&engine, &out, Map::new()).expect("write");
        let playback: Json = serde_json::from_str(&std::fs::read_to_string(out.join("playback.json")).unwrap()).unwrap();
        let scene = rbx_binary::from_reader(std::fs::read(out.join("scene.rbxm")).unwrap().as_slice()).expect("scene.rbxm");
        let _ = std::fs::remove_dir_all(&out);
        drop(lua);
        engine.teardown();
        (playback, scene, summary)
    };
    let sets = |playback: &Json, property: &str| -> Vec<(f64, Json)> {
        find(&playback["actions"], |a| a["target"] == "Workspace.Terrain.Clouds" && !a["set"][property].is_null()).iter().map(|a| (a["at"].as_f64().unwrap(), a["set"][property].clone())).collect()
    };
    let (playback, scene, summary) = run(
        r#"
        local Clouds = workspace.Terrain:FindFirstChildOfClass("Clouds")
        local Held = Clouds.Enabled
        Clouds.Enabled = Held
        Clouds.Color = Color3.fromRGB(70, 74, 88)
        Clouds.Cover = 0.95
        local Stray = Instance.new("Clouds")
        Stray.Parent = workspace
        Stray.Cover = 0.1
        task.delay(0.1, function()
            Clouds.Cover = 0.95
            game:GetService("TweenService"):Create(Clouds, TweenInfo.new(0.05, Enum.EasingStyle.Linear), {Density = 0.2}):Play()
        end)
        "#,
        true,
    );
    let enabled = sets(&playback, "Enabled");
    assert_eq!(enabled, vec![(0.0, Json::Bool(true))], "the held value written back plays at frame 0");
    let color = sets(&playback, "Color");
    assert_eq!(color.len(), 1, "{color:?}");
    let rgb: Vec<f64> = color[0].1.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(color[0].0 == 0.0 && (rgb[0] - 70.0 / 255.0).abs() < 1e-4 && (rgb[2] - 88.0 / 255.0).abs() < 1e-4, "{color:?}");
    let cover = sets(&playback, "Cover");
    assert_eq!(cover.len(), 1, "a write of the value the run last wrote plays nothing: {cover:?}");
    assert!(cover[0].0 == 0.0 && (cover[0].1.as_f64().unwrap() - 0.95).abs() < 1e-5, "{cover:?}");
    let tweens = find(&playback["tweens"], |t| t["target"] == "Workspace.Terrain.Clouds");
    assert_eq!(tweens.len(), 1, "{tweens:?}");
    assert_eq!(tweens[0]["property"], "Density");
    let keys = tweens[0]["keys"].as_array().unwrap();
    assert_eq!(keys[0]["value"], serde_json::json!({ "initial": true }), "{keys:?}");
    assert!(keys[0]["at"].as_f64().unwrap() > 0.0, "{keys:?}");
    assert!((keys.last().unwrap()["value"].as_f64().unwrap() - 0.2).abs() < 1e-5, "{keys:?}");
    assert!(find(&playback["actions"], |a| a["target"] != "Workspace.Terrain.Clouds").is_empty(), "the stray Clouds plays nothing: {}", playback["actions"]);
    assert!(scene.descendants().all(|instance| instance.class != "Clouds"), "no Clouds is recorded as an instance");
    assert!(summary["unsupported"].as_array().unwrap().is_empty(), "{summary}");

    // clouds the run makes in the Terrain are the Terrain's too
    let (playback, scene, _) = run(
        r#"
        local Clouds = Instance.new("Clouds")
        Clouds.Cover = 0.3
        Clouds.Parent = workspace.Terrain
        "#,
        false,
    );
    let cover = sets(&playback, "Cover");
    assert!(cover.len() == 1 && cover[0].0 == 0.0 && (cover[0].1.as_f64().unwrap() - 0.3).abs() < 1e-5, "{cover:?}");
    assert!(scene.descendants().all(|instance| instance.class != "Clouds"), "no Clouds is recorded as an instance");
}

/// A Glass part still draws at Transparency 1 when a Highlight is under it (ror's notes, glass-quirks),
/// so one that appears later and is destroyed leaves the world before and after instead of turning
/// Transparency 1: the house's distortion bubbles (a disabled Highlight under each) otherwise hung on
/// at their last size, hiding the windows, particles and GUIs behind them for the rest of the run.
#[test]
fn glass_parts_leave_the_world_while_they_are_gone() {
    let (playback, _, scene, _) = record(
        r#"
        local Folder = Instance.new("Folder")
        Folder.Name = "Effect"
        Folder.Parent = workspace
        task.delay(0.1, function()
            local Bubble = Instance.new("Part")
            Bubble.Name = "Bubble"
            Bubble.Shape = Enum.PartType.Ball
            Bubble.Material = Enum.Material.Glass
            Bubble.Transparency = 3
            local Highlight = Instance.new("Highlight")
            Highlight.Enabled = false
            Highlight.Parent = Bubble
            Bubble.Parent = Folder
            local Pane = Instance.new("Part")
            Pane.Name = "Pane"
            Pane.Parent = Folder
            task.delay(0.1, function()
                Bubble:Destroy()
                Pane:Destroy()
            end)
        end)
        "#,
        20,
    );
    let bubble = instance(&scene, &["Effect", "Bubble"]);
    assert_eq!(bubble.properties.get(&"Transparency".into()), Some(&Variant::Float32(3.0)), "the file keeps its own transparency");
    let moves: Vec<(f64, String)> = find(&playback["actions"], |a| a["target"] == "Effect.Bubble" && a["parent"].is_string())
        .iter()
        .map(|a| (a["at"].as_f64().unwrap(), a["parent"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(moves.len(), 3, "out at the start, in when it appears, out when destroyed: {moves:?}");
    assert!(moves[0].0 == 0.0 && moves[0].1.is_empty(), "{moves:?}");
    assert!((moves[1].0 - 0.1).abs() < 0.02 && moves[1].1 == "Effect", "{moves:?}");
    assert!((moves[2].0 - 0.2).abs() < 0.03 && moves[2].1.is_empty(), "{moves:?}");
    let hides = find(&playback["actions"], |a| a["target"] == "Effect.Bubble" && a["set"]["Transparency"].is_number());
    assert!(hides.is_empty(), "Glass is not hidden by Transparency: {hides:?}");
    // a part of another material still hides by Transparency 1
    let pane: Vec<f64> = find(&playback["actions"], |a| a["target"] == "Effect.Pane" && a["set"]["Transparency"].is_number())
        .iter()
        .map(|a| a["set"]["Transparency"].as_f64().unwrap())
        .collect();
    assert_eq!(pane, [0.0, 1.0]);
}

/// A post effect in the camera draws as one in Lighting (measured in Studio), so it plays there:
/// an effect can light its impact with a BloomEffect in workspace.CurrentCamera. The camera's
/// other children stay roots of the scene.
#[test]
fn camera_post_effects_play_in_the_lighting() {
    let (playback, _, scene, _) = record(
        r#"
        local Camera = workspace.CurrentCamera
        local Bloom = Instance.new("BloomEffect")
        Bloom.Name = "ImpactBloom"
        Bloom.Enabled = false
        Bloom.Parent = Camera
        local Part = Instance.new("Part")
        Part.Name = "Near"
        Part.Parent = Camera
        task.delay(0.1, function()
            Bloom.Enabled = true
            task.delay(0.1, function()
                Bloom:Destroy()
            end)
        end)
        "#,
        20,
    );
    instance(&scene, &["~Lighting", "ImpactBloom"]);
    instance(&scene, &["Near"]);
    let parents = find(&playback["actions"], |a| a["target"] == "~Lighting.ImpactBloom" && a["parent"].is_string());
    let moves: Vec<&str> = parents.iter().map(|a| a["parent"].as_str().unwrap()).collect();
    assert_eq!(moves, ["Lighting", "~Lighting"]);
    assert_eq!(find(&playback["actions"], |a| a["target"] == "~Lighting.ImpactBloom" && a["set"]["Enabled"] == true).len(), 1);
}

/// The place's own look parked by the run (as a cutscene parks the map's look) takes ror's
/// stage of its class out of Lighting and back with it; one the run made and parked before a frame
/// showed it does too, and is recorded itself when it comes.
#[test]
fn the_places_own_look_leaves_with_the_run() {
    let (playback, _, scene, _, _) = record_in(
        r#"
        local Lighting = game:GetService("Lighting")
        local Grading = Lighting:FindFirstChild("Grading")
        local Sky = Lighting:FindFirstChild("Sky")
        local Template = Instance.new("BloomEffect")
        Template.Name = "MapBloom"
        Template.Parent = Lighting
        Template.Parent = nil
        task.delay(0.1, function()
            Grading.Parent = nil
            Sky.Parent = nil
            local Own = Instance.new("ColorCorrectionEffect")
            Own.Name = "Cutscene"
            Own.Parent = Lighting
            task.delay(0.1, function()
                Grading.Parent = Lighting
                Template.Parent = Lighting
            end)
        end)
        "#,
        20,
        false,
        &[("ColorCorrectionEffect", "Grading"), ("Sky", "Sky"), ("Atmosphere", "Air")],
    );
    let stage: Vec<(String, String, f64)> = find(&playback["actions"], |a| a["target"] == "Lighting" && a["class"].is_string())
        .iter()
        .map(|a| (a["class"].as_str().unwrap().to_string(), a["parent"].as_str().unwrap().to_string(), a["at"].as_f64().unwrap()))
        .collect();
    let stage: Vec<(&str, &str, bool)> = stage.iter().map(|(class, parent, at)| (class.as_str(), parent.as_str(), *at > 0.15)).collect();
    assert_eq!(stage, [("BloomEffect", "", false), ("ColorCorrectionEffect", "", false), ("Sky", "", false), ("ColorCorrectionEffect", "Lighting", true)]);
    instance(&scene, &["~Lighting", "Cutscene"]);
    instance(&scene, &["~Lighting", "MapBloom"]);
}

/// A part that stays put under a moving part is pinned with a still track, but an attachment or a
/// bone under it is not: ror carries it with its part (pinned bones left a skinned blade
/// and cloth rigs at a walking character's spawn).
#[test]
fn bones_and_attachments_go_with_their_part() {
    let (playback, _, _, _) = record(
        r#"
        local Model = Instance.new("Model")
        Model.Name = "Rig"
        local Root = Instance.new("Part")
        Root.Name = "Root"
        Root.Anchored = true
        Root.Parent = Model
        local Blade = Instance.new("Part")
        Blade.Name = "Blade"
        Blade.Parent = Root
        local Bone = Instance.new("Bone")
        Bone.Name = "Bone"
        Bone.Position = Vector3.new(0, 1, 0)
        Bone.Parent = Blade
        local Tip = Instance.new("Bone")
        Tip.Name = "Tip"
        Tip.Position = Vector3.new(0, 1, 0)
        Tip.Parent = Bone
        local Grip = Instance.new("Attachment")
        Grip.Name = "Grip"
        Grip.Parent = Blade
        Model.Parent = workspace
        task.delay(0.05, function()
            for Step = 1, 5 do
                Root.CFrame = CFrame.new(Step, 0, 0)
                task.wait()
            end
        end)
        "#,
        20,
    );
    let tracked = |target: &str| !find(&playback["motion"], |m| m["target"] == target).is_empty();
    assert!(tracked("Rig.Root"), "the root's motion: {}", playback["motion"]);
    assert!(tracked("Rig.Root.Blade"), "a part under a moving part keeps its still track");
    for target in ["Rig.Root.Blade.Bone", "Rig.Root.Blade.Bone.Tip", "Rig.Root.Blade.Grip"] {
        assert!(!tracked(target), "{target} was pinned where it appeared: {}", playback["motion"]);
    }
}

/// A bone is drawn where its Transform puts it (TransformedWorldCFrame), as an Animator's pose
/// moves skinned meshes: one posed before it appears and one posed every frame after, while the
/// bone under it is carried by its parent bone.
#[test]
fn bones_are_recorded_with_their_transform() {
    let (playback, _, _, _) = record(
        r#"
        local Model = Instance.new("Model")
        Model.Name = "Rig"
        local Root = Instance.new("Part")
        Root.Name = "Root"
        Root.Anchored = true
        Root.CFrame = CFrame.new(10, 0, 0)
        Root.Parent = Model
        local Posed = Instance.new("Bone")
        Posed.Name = "Posed"
        Posed.Position = Vector3.new(0, 1, 0)
        Posed.Transform = CFrame.new(0, 0, 3)
        Posed.Parent = Root
        local Arm = Instance.new("Bone")
        Arm.Name = "Arm"
        Arm.Position = Vector3.new(0, 2, 0)
        Arm.Parent = Root
        local Hand = Instance.new("Bone")
        Hand.Name = "Hand"
        Hand.Position = Vector3.new(0, 1, 0)
        Hand.Parent = Arm
        Model.Parent = workspace
        task.delay(0.05, function()
            for Step = 1, 5 do
                Arm.Transform = CFrame.new(Step, 0, 0)
                task.wait()
            end
        end)
        "#,
        20,
    );
    let keys = |target: &str| find(&playback["motion"], |m| m["target"] == target).first().map(|m| m["keys"].clone()).unwrap_or(Json::Null);
    let position = |key: &Json| [0, 1, 2].map(|i| key["cframe"][i].as_f64().unwrap());
    let posed = keys("Rig.Root.Posed");
    assert!(posed.is_array(), "the pose it appeared in was not recorded: {}", playback["motion"]);
    assert_eq!(position(&posed[0]), [10.0, 1.0, 3.0], "the pose it appeared in: {posed}");
    let arm = keys("Rig.Root.Arm");
    let last = arm.as_array().and_then(|keys| keys.last()).unwrap_or_else(|| panic!("the arm's Transform was not recorded: {}", playback["motion"]));
    assert_eq!(position(last), [15.0, 2.0, 0.0], "{arm}");
    assert!(keys("Rig.Root.Arm.Hand").is_null(), "the hand goes with its parent bone: {}", playback["motion"]);
}

/// A bone whose own track starts late rode its part until then: its track has those frames, not a
/// hold at its first sample while the part walks away.
#[test]
fn a_bone_whose_track_starts_late_rode_its_part_until_then() {
    let (playback, _, _, _) = record(
        r#"
        local Model = Instance.new("Model")
        Model.Name = "Rig"
        local Root = Instance.new("Part")
        Root.Name = "Root"
        Root.Anchored = true
        Root.CFrame = CFrame.new(0, 0, 0)
        Root.Parent = Model
        local Blade = Instance.new("Bone")
        Blade.Name = "Blade"
        Blade.Position = Vector3.new(0, 1, 0)
        Blade.Parent = Root
        Model.Parent = workspace
        task.spawn(function()
            for Step = 1, 18 do
                task.wait()
                Root.CFrame = CFrame.new(Step, 0, 0)
                if Step >= 12 then
                    Blade.Transform = CFrame.new(0, 0, Step - 11)
                end
            end
        end)
        "#,
        24,
    );
    let keys = find(&playback["motion"], |m| m["target"] == "Rig.Root.Blade").first().map(|m| m["keys"].clone()).unwrap_or(Json::Null);
    let keys = keys.as_array().unwrap_or_else(|| panic!("the blade has no track: {}", playback["motion"]));
    // (keys a straight line reproduces are dropped: the playback interpolates)
    let at = |time: f64| {
        let position = |key: &Json| [0, 1, 2].map(|i| key["cframe"][i].as_f64().unwrap());
        let time_of = |key: &Json| key["at"].as_f64().unwrap();
        let after = keys.iter().position(|key| time_of(key) > time).unwrap_or(keys.len());
        if after == 0 {
            return position(&keys[0]);
        }
        if after == keys.len() {
            return position(&keys[keys.len() - 1]);
        }
        let (a, b) = (&keys[after - 1], &keys[after]);
        let alpha = (time - time_of(a)) / (time_of(b) - time_of(a));
        let (pa, pb) = (position(a), position(b));
        [0, 1, 2].map(|i| pa[i] + (pb[i] - pa[i]) * alpha)
    };
    let first = keys[0]["at"].as_f64().unwrap();
    assert!(first <= 1.5 / 60.0, "the track starts where the blade appeared, not where its Transform was first set: {keys:?}");
    let early = at(6.0 / 60.0);
    assert!(early[0] > 3.0 && early[0] < 7.0 && (early[1] - 1.0).abs() < 1e-4 && early[2].abs() < 1e-4, "the blade rode its part before its own track: {early:?} {keys:?}");
    let last = at(1.0);
    assert_eq!(last, [18.0, 1.0, 7.0], "{keys:?}");
}

/// `ree run` prints the same bytes whether its output lines are kept or written to a temporary file
/// as they come (the command line's run: a write trace's ~600k lines are not kept in memory).
#[test]
fn a_spooled_run_prints_what_a_kept_one_prints() {
    let root = std::env::temp_dir().join(format!("ree-spool-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("none")).unwrap();
    let harness = root.join("Lines.luau");
    std::fs::write(
        &harness,
        "return function(Context)\n\
         print('plain', 1.5, nil, Context.Scenario)\n\
         print('quote \\\" backslash \\\\ tab \\t newline \\n unicode \u{e9} \u{2713} \u{1F600} nul \\0 end')\n\
         warn('a warning')\n\
         task.spawn(function() error('raised in a thread') end)\n\
         for Index = 1, 3000 do print('line', Index, string.rep('x', Index % 97)) end\n\
         task.wait(0.05)\n\
         print('after a wait')\n\
         end",
    )
    .unwrap();
    let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.1", "--context", r#"{"Scenario":"spool"}"#]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let parsed = super::run::parse(&args, false).expect("arguments");
    let host = crate::launch::Host::default();
    let print = |spool: bool| {
        let (done, emulator) = super::run::execute_with(&parsed, &host, false, spool).expect("run");
        assert_eq!(done.output.is_some(), spool, "spooled: {spool}");
        assert!(done.line.contains("3005 output lines; 1 errors"), "{}", done.line);
        let mut bytes = Vec::new();
        done.print(&mut bytes).expect("printed");
        drop(emulator);
        String::from_utf8(bytes).expect("UTF-8")
    };
    let (kept, spooled) = (print(false), print(true));
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(kept, spooled);
    let summary: Json = serde_json::from_str(&kept).expect("one line of JSON");
    let output = summary["output"].as_array().expect("output");
    assert_eq!((output.len(), summary["errors"].as_u64()), (3005, Some(1)));
    assert!(output.iter().any(|line| line["kind"] == "error" && line["trace"].is_string()), "{kept:.2000}");
    assert!(output[1]["message"].as_str().unwrap().contains("\u{1F600}"), "{}", output[1]);
    assert!(kept.ends_with("}\n") && kept.matches('\n').count() == 1);
}

/// `ree record --timing-check` probes faster and slower clients: a script whose look turns on a threshold of os.clock within
/// a frame hinges under the probe on the far side of it, whichever direction that is (the check before probed only a clock
/// twice as slow, so a threshold a faster client misses, or one only a phone crosses, read "no change"), and the work each
/// frame did is compared too
#[test]
fn a_timing_check_probes_faster_and_slower_clients() {
    let root = std::env::temp_dir().join(format!("ree-record-probes-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("none")).unwrap();
    let host = crate::launch::Host::default();
    let record = |harness: &str, name: &str, timing: bool| {
        let path = root.join(format!("{name}.luau"));
        std::fs::write(&path, harness).unwrap();
        let out = root.join(name);
        let mut args: Vec<String> = ["--harness", &path.to_string_lossy(), "--out", &out.to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.1", "--fps", "60"].iter().map(|s| s.to_string()).collect();
        if timing {
            args.push("--timing-check".into());
        }
        let parsed = super::run::parse(&args, true).expect("arguments");
        let (done, emulator) = super::run::execute_with(&parsed, &host, true, false).expect("record");
        drop(emulator);
        (done, out)
    };
    // how long the work reads on os.clock as modelled (the work clock's prices, whatever they are)
    let work = "local Start = os.clock() local X = 0 for I = 1, 20000 do X += I % 7 end local Elapsed = os.clock() - Start";
    // (in a frame: work before the first frame, a harness's build, is not charged)
    let (_, out) = record(&format!("return function(Context) task.wait() {work} print(string.format('%.9f', Elapsed)) end"), "measure", false);
    let events: Json = serde_json::from_str(&std::fs::read_to_string(out.join("events.json")).unwrap()).unwrap();
    let modelled: f64 = events["output"].as_array().unwrap().iter().find_map(|line| line["message"].as_str()?.parse().ok()).expect("the elapsed time printed");
    assert!(modelled > 0.0, "the loop moves os.clock");
    // a threshold at 1.5x the modelled work: only a slower client crosses it; at 0.9x: only a faster one falls short
    let threshold = |name: &str, factor: f64| {
        let harness = format!(
            "return function(Context) local Part = Instance.new('Part') Part.Anchored = true Part.Parent = workspace task.wait() {work} Part.Transparency = if Elapsed > {:.12} then 0.5 else 0 task.wait() end",
            modelled * factor
        );
        record(&harness, name, true).0
    };
    let slower = threshold("slower", 1.5);
    let faster = threshold("faster", 0.9);
    let _ = std::fs::remove_dir_all(&root);
    let probes = |done: &super::run::Done| -> Vec<(f64, bool)> { done.summary["timing"]["probes"].as_array().unwrap().iter().map(|probe| (probe["factor"].as_f64().unwrap(), probe["hinges"] == true)).collect() };
    // Current calibration: inverse fidelity endpoints (1/1.34, 1/0.55), then the slowest phone (4.16/0.55).
    assert_eq!(probes(&slower).iter().map(|probe| probe.0).collect::<Vec<_>>(), vec![0.746, 1.818, 7.564]);
    assert_eq!(probes(&slower).iter().map(|probe| probe.1).collect::<Vec<_>>(), vec![false, true, true], "{}", slower.summary["timing"]);
    assert_eq!(probes(&faster).iter().map(|probe| probe.1).collect::<Vec<_>>(), vec![true, false, false], "{}", faster.summary["timing"]);
    assert!(faster.line.contains("hinges on sub-frame timing under 0.75x (the fastest desktop client)") && faster.line.contains("no change at 1.82x, 7.56x"), "{}", faster.line);
    assert_eq!(slower.summary["timing"]["range"]["factors"], serde_json::json!([0.746, 7.564]));
    assert_eq!(faster.summary["timing"]["first"]["list"], "actions");
}

/// `ree record --timing-check`: a recording whose script budgets its work on os.clock within a frame hinges on sub-frame
/// timing (a probe recording with the in-frame clock moved differs, from the first changed key, in what it draws and in the
/// work its frames do), one that does not reads no change; the verdict is in the summary and timing.json, and the probes'
/// files are gone.
#[test]
fn a_timing_check_says_whether_a_recording_hinges_on_sub_frame_timing() {
    let root = std::env::temp_dir().join(format!("ree-record-timing-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("none")).unwrap();
    let write = |name: &str, body: &str| {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        path
    };
    let steady = write(
        "Steady.luau",
        "return function(Context)\n\
         local Part = Instance.new('Part') Part.Anchored = true Part.Parent = workspace\n\
         game:GetService('TweenService'):Create(Part, TweenInfo.new(0.2), {Transparency = 0.5}):Play()\n\
         task.wait(0.25)\n\
         end",
    );
    let budgeted = write(
        "Budgeted.luau",
        "return function(Context)\n\
         local Part = Instance.new('Part') Part.Anchored = true Part.Parent = workspace\n\
         local Steps = 0\n\
         for _ = 1, 10 do\n\
         local Start = os.clock()\n\
         repeat for _ = 1, 200 do local _ = Part.Position end Steps += 1 until os.clock() - Start > 0.0005 or Steps % 50 == 0\n\
         Part.Position = Vector3.new(Steps, 0, 0)\n\
         task.wait()\n\
         end\n\
         end",
    );
    let host = crate::launch::Host::default();
    let check = |harness: &std::path::Path, name: &str| {
        let out = root.join(name);
        let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--out", &out.to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.3", "--fps", "60", "--timing-check"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = super::run::parse(&args, true).expect("arguments");
        let (done, emulator) = super::run::execute_with(&parsed, &host, true, false).expect("record");
        drop(emulator);
        let written: Json = serde_json::from_str(&std::fs::read_to_string(out.join("timing.json")).unwrap()).unwrap();
        assert_eq!(written, done.summary["timing"]);
        assert!((0..3).all(|index| !out.join(format!("timing-check-{index}")).exists()), "the probes' files are removed");
        done
    };
    let steady = check(&steady, "steady");
    assert_eq!(steady.summary["timing"]["hinges"], Json::Bool(false), "{}", steady.summary["timing"]);
    assert!(steady.line.contains("sub-frame timing: no change (tested 0.75x to 7.56x the modelled clock"), "{}", steady.line);
    let budgeted = check(&budgeted, "budgeted");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(budgeted.summary["timing"]["hinges"], Json::Bool(true), "{}", budgeted.summary["timing"]);
    assert_eq!(budgeted.summary["timing"]["first"]["what"], "Part");
    assert!(budgeted.line.contains("hinges on sub-frame timing under"), "{}", budgeted.line);
    // (the steps a frame does under the budget: its work moved too)
    let probes = budgeted.summary["timing"]["probes"].as_array().unwrap();
    assert!(probes.iter().any(|probe| probe["differing"]["frame work"].as_u64().unwrap_or(0) > 0), "{}", budgeted.summary["timing"]);
}

/// `ree record` writes the same files whether its output lines are kept or written to a temporary file
/// as events.json holds them, as they come (the command line's recording).
#[test]
fn a_spooled_recording_writes_what_a_kept_one_writes() {
    let root = std::env::temp_dir().join(format!("ree-record-spool-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("none")).unwrap();
    let harness = root.join("Lines.luau");
    std::fs::write(
        &harness,
        "return function(Context)\n\
         local Part = Instance.new('Part') Part.Anchored = true Part.Parent = workspace\n\
         print('plain', 1.5, nil, Context.Scenario)\n\
         print('quote \\\" backslash \\\\ tab \\t newline \\n unicode \u{e9} \u{2713} \u{1F600} end')\n\
         warn('a warning')\n\
         task.spawn(function() error('raised in a thread') end)\n\
         for Index = 1, 2000 do print('line', Index, string.rep('x', Index % 61)) end\n\
         task.wait(0.05)\n\
         Part.Position = Vector3.new(1, 2, 3)\n\
         game:GetService('TweenService'):Create(Part, TweenInfo.new(0.05), {Transparency = 0.5}):Play()\n\
         game:GetService('RunService').Heartbeat:Connect(function(Step) Part.CFrame = Part.CFrame * CFrame.new(Step, 0, 0) end)\n\
         local Emitter = Instance.new('ParticleEmitter') Emitter.Parent = Part Emitter:Emit(3)\n\
         warn('after a wait')\n\
         end",
    )
    .unwrap();
    let host = crate::launch::Host::default();
    let record = |spool: bool| {
        let out = root.join(if spool { "spooled" } else { "kept" });
        let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--out", &out.to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.1", "--fps", "30", "--context", r#"{"Scenario":"spool"}"#]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = super::run::parse(&args, true).expect("arguments");
        let (done, emulator) = super::run::execute_with(&parsed, &host, true, spool).expect("record");
        drop(emulator);
        assert_eq!((done.summary["errors"].as_u64(), done.summary["warnings"].as_u64()), (Some(1), Some(2)), "{}", done.summary);
        ["scene.rbxm", "playback.json", "events.json", "camera.json", "summary.json"].map(|name| {
            let bytes = std::fs::read(out.join(name)).unwrap();
            if name == "summary.json" {
                let mut summary: Json = serde_json::from_slice(&bytes).unwrap();
                summary["out"] = Json::Null;
                return serde_json::to_vec_pretty(&summary).unwrap();
            }
            bytes
        })
    };
    let (kept, spooled) = (record(false), record(true));
    let _ = std::fs::remove_dir_all(&root);
    for (index, name) in ["scene.rbxm", "playback.json", "events.json", "camera.json", "summary.json"].iter().enumerate() {
        assert!(kept[index] == spooled[index], "{name} differs:\n{}\n---\n{}", String::from_utf8_lossy(&kept[index][..kept[index].len().min(800)]), String::from_utf8_lossy(&spooled[index][..spooled[index].len().min(800)]));
    }
    let events: Json = serde_json::from_slice(&kept[2]).unwrap();
    assert_eq!(events["output"].as_array().map(Vec::len), Some(2005), "{events:.400}");
    // (written piece by piece: each file is the pretty JSON of its own value)
    for index in [1, 2] {
        let value: Json = serde_json::from_slice(&spooled[index]).unwrap();
        assert!(serde_json::to_vec_pretty(&value).unwrap() == spooled[index], "file {index} is not its value's pretty JSON");
    }
    let playback: Json = serde_json::from_slice(&kept[1]).unwrap();
    for key in ["actions", "tweens", "motion", "frames"] {
        assert!(playback[key].as_array().is_some_and(|list| !list.is_empty()), "{key}: {playback:.600}");
    }
}

#[test]
fn a_harness_requires_its_own_modules() {
    let root = std::env::temp_dir().join(format!("ree-harness-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let write = |relative: &str, text: &str| {
        let file = root.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write("Beside.luau", "local Parts = require(script.Parts) return function() print(script.Name, Parts.Name, require(script.Parts.Deep).Value, require(script.Data.Kind).Value, script.Data.ClassName) end");
    write("Beside/Parts/init.luau", "return {Name = 'parts'}");
    write("Beside/Parts/Deep.luau", "return {Value = 3}");
    write("Beside/Data/Kind.luau", "return {Value = 'data'}");
    write("Packed/init.luau", "return function() print(script.Name, require(script.Helper)) end");
    write("Packed/Helper.luau", "return 'helper'");
    write("Alone.luau", "return function() print(script.Name, #script:GetChildren()) end");

    let mut lines = Vec::new();
    for harness in ["Beside.luau", "Packed/init.luau", "Alone.luau"] {
        let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
        let emulator = crate::Emulator::start(engine, &crate::mount::ProjectSpec::default()).expect("emulator");
        let args = super::run::RecordArgs {
            engine: crate::launch::Args::default(),
            harness: root.join(harness).to_string_lossy().into_owned(),
            out: None,
            seconds: 1.0 / 60.0,
            fps: 60.0,
            context: None,
            model_assets: Default::default(),
            animations: Vec::new(),
            frame_times: None,
            script_timeout: None,
            script_timeouts: None,
            timing_check: false,
            clock_probe: None,
        };
        let ran = super::run::run(&emulator.lua, &emulator.engine, &args, None).expect("run");
        assert_eq!(ran.status, "ok", "{harness}: {:?}", ran.failure);
        lines.extend(emulator.output().iter().map(|line| format!("{}: {}", line.kind, line.message)));
    }
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(lines, ["print: Beside parts 3 data Folder", "print: Packed helper", "print: Alone 0"]);
}

#[test]
fn what_a_client_parents_to_its_camera_is_recorded() {
    let (playback, _, scene, _) = record(
        r#"
        local World = Instance.new("Folder")
        World.Name = "ClientWorld"
        World.Parent = workspace.CurrentCamera
        task.delay(0.05, function()
            local Part = Instance.new("Part")
            Part.Name = "Spark"
            Part.Parent = World
            local Emitter = Instance.new("ParticleEmitter")
            Emitter.Parent = Part
            Emitter:Emit(3)
        end)
        "#,
        10,
    );
    instance(&scene, &["ClientWorld", "Spark", "ParticleEmitter"]);
    assert!(scene.root().children().iter().all(|r| scene.get_by_ref(*r).unwrap().class != "Camera"), "the camera itself is not recorded");
    let emits = find(&playback["actions"], |a| a["emit"].is_number());
    assert_eq!(emits.len(), 1);
    assert_eq!(emits[0]["target"], "ClientWorld.Spark.ParticleEmitter");
}

/// What a script parents to workspace.Terrain draws as Studio draws it (an effect module can clone its
/// meshes into Terrain): recorded as roots, the Terrain itself not.
#[test]
fn what_a_script_parents_to_terrain_is_recorded() {
    let (playback, _, scene, _) = record(
        r#"
        task.delay(0.05, function()
            local Ring = Instance.new("Part")
            Ring.Name = "Ring"
            Ring.Material = Enum.Material.Neon
            Ring.Parent = workspace.Terrain
            Ring.Transparency = 0.5
            local Emitter = Instance.new("ParticleEmitter")
            Emitter.Parent = Ring
            Emitter:Emit(2)
        end)
        "#,
        10,
    );
    instance(&scene, &["Ring", "ParticleEmitter"]);
    assert!(scene.root().children().iter().all(|r| scene.get_by_ref(*r).unwrap().class != "Terrain"), "the Terrain itself is not recorded");
    let emits = find(&playback["actions"], |a| a["emit"].is_number());
    assert_eq!(emits.len(), 1);
    assert_eq!(emits[0]["target"], "Ring.ParticleEmitter");
}

/// A portal window built as EgoMoose's ViewportWindow builds one (an effect's portal):
/// a SurfaceGui in the PlayerGui adorned to a part in the world, a ViewportFrame whose camera a
/// script moves every frame. It is recorded under the part it draws on, the frame's camera as Roblox
/// files keep it (CameraCFrame, CameraFieldOfView) and its moves in the playback.
#[test]
fn a_player_gui_window_is_recorded_under_its_adornee() {
    let (playback, _, scene, _) = record(
        r#"
        local Portal = Instance.new("Part")
        Portal.Name = "Portal"
        Portal.Parent = workspace
        local Window = Instance.new("SurfaceGui")
        Window.Name = "Window"
        Window.Adornee = Portal
        Window.Parent = game:GetService("Players").LocalPlayer:WaitForChild("PlayerGui")
        local Camera = Instance.new("Camera")
        Camera.Name = "WorldCamera"
        Camera.Parent = Window
        local Frame = Instance.new("ViewportFrame")
        Frame.CurrentCamera = Camera
        Frame.Parent = Window
        local World = Instance.new("WorldModel")
        World.Parent = Frame
        local Inside = Instance.new("Part")
        Inside.Name = "Inside"
        Inside.Parent = World
        game:GetService("RunService").RenderStepped:Connect(function()
            Camera.CFrame = Camera.CFrame + Vector3.new(0, 0, 1)
        end)
        "#,
        8,
    );
    instance(&scene, &["Portal", "Window", "ViewportFrame", "WorldModel", "Inside"]);
    let frame = instance(&scene, &["Portal", "Window", "ViewportFrame"]);
    assert!(matches!(frame.properties.get(&rbx_dom_weak::ustr("CameraFieldOfView")), Some(Variant::Float32(fov)) if *fov == 70.0), "{:?}", frame.properties.get(&rbx_dom_weak::ustr("CameraFieldOfView")));
    let moves = playback["tweens"].as_array().unwrap().iter().chain(playback["actions"].as_array().unwrap()).filter(|entry| entry["target"] == "Portal.Window.ViewportFrame" && (entry["property"] == "CameraCFrame" || entry["set"]["CameraCFrame"].is_array())).count();
    assert!(moves > 0, "the camera's moves are in the playback: {playback}");
    assert_eq!(playback["play"], true, "a recording of play");
}

/// BillboardGuis in the PlayerGui draw on whatever they are adorned to in the world, as Roblox draws
/// them: an invisible HumanoidRootPart (an emote can hang its portrait on one), an attachment, a
/// part in the CurrentCamera, a part that enters the world after the GUI. The Adornee may be written
/// after the GUI is parented, in a frame where nothing else moves (it appears then), cleared (it
/// leaves) or changed (the playback names the new one).
#[test]
fn player_gui_billboards_draw_on_any_adornee_in_the_world() {
    let (playback, _, scene, _) = record(
        r#"
        local PlayerGui = game:GetService("Players").LocalPlayer:WaitForChild("PlayerGui")
        local function Billboard(Name, Adornee)
            local Gui = Instance.new("BillboardGui")
            Gui.Name = Name
            Gui.Size = UDim2.fromOffset(100, 100)
            Gui.Adornee = Adornee
            Instance.new("Frame").Parent = Gui
            Gui.Parent = PlayerGui
            return Gui
        end
        local Character = Instance.new("Model")
        Character.Name = "Character"
        local Root = Instance.new("Part")
        Root.Name = "HumanoidRootPart"
        Root.Transparency = 1
        Root.Parent = Character
        Instance.new("Humanoid").Parent = Character
        local RootAttachment = Instance.new("Attachment")
        RootAttachment.Name = "RootAttachment"
        RootAttachment.Parent = Root
        Character.Parent = workspace
        local Local = Instance.new("Model")
        Local.Name = "Local"
        local Ghost = Instance.new("Part")
        Ghost.Name = "Ghost"
        Ghost.Transparency = 1
        Ghost.Parent = Local
        Local.Parent = workspace.CurrentCamera
        local Spawned = Instance.new("Part")
        Spawned.Name = "Spawned"
        Billboard("OnRoot", Root)
        Billboard("OnAttachment", RootAttachment)
        Billboard("OnCameraPart", Ghost)
        Billboard("Early", Spawned)
        local Late = Billboard("Late", nil)
        local Cleared = Billboard("Cleared", Root)
        local Moved = Billboard("Moved", Root)
        task.delay(0.05, function() Spawned.Parent = workspace end)
        task.delay(0.1, function()
            Late.Adornee = Root
            Cleared.Adornee = nil
        end)
        task.delay(0.15, function() Moved.Adornee = Ghost end)
        "#,
        12,
    );
    for path in [
        &["Character", "HumanoidRootPart", "OnRoot"][..],
        &["Character", "HumanoidRootPart", "RootAttachment", "OnAttachment"],
        &["Local", "Ghost", "OnCameraPart"],
        &["Spawned", "Early"],
        &["Character", "HumanoidRootPart", "Late", "Frame"],
        &["Character", "HumanoidRootPart", "Cleared"],
        &["Character", "HumanoidRootPart", "Moved"],
    ] {
        assert_eq!(instance(&scene, path).class, if path.last() == Some(&"Frame") { "Frame" } else { "BillboardGui" });
    }
    let enabled = |target: &str| -> Vec<(f64, bool)> {
        find(&playback["actions"], |a| a["target"] == target && a["set"]["Enabled"].is_boolean()).iter().map(|a| (a["at"].as_f64().unwrap(), a["set"]["Enabled"].as_bool().unwrap())).collect()
    };
    assert_eq!(enabled("Spawned.Early"), [(0.05, true)], "{playback}");
    assert_eq!(enabled("Character.HumanoidRootPart.Late"), [(0.1, true)], "appears once adorned: {playback}");
    assert_eq!(enabled("Character.HumanoidRootPart.Cleared"), [(0.1, false)], "leaves once its Adornee is cleared: {playback}");
    let moved = find(&playback["actions"], |a| a["target"] == "Character.HumanoidRootPart.Moved" && a["set"]["Adornee"].is_string());
    assert!(moved.len() == 1 && moved[0]["set"]["Adornee"] == "Local.Ghost" && moved[0]["at"] == 0.15, "{playback}");
}

/// A cutscene's fade (a transition): a ScreenGui parented to the PlayerGui, its full-screen
/// Frame tweened black and back, the GUI destroyed after. It is recorded in the scene's PlayerGui with
/// its tween and hidden (Enabled off) once gone; a ScreenGui in a Folder of the PlayerGui is recorded
/// beside it, and a SurfaceGui inside one adorned to a part still draws on that part.
#[test]
fn player_gui_screen_guis_are_recorded_in_the_scenes_player_gui() {
    let (playback, _, scene, _) = record(
        r#"
        local TweenService = game:GetService("TweenService")
        local PlayerGui = game:GetService("Players").LocalPlayer:WaitForChild("PlayerGui")
        local Portal = Instance.new("Part")
        Portal.Name = "Portal"
        Portal.Parent = workspace
        task.delay(0.05, function()
            local Transition = Instance.new("ScreenGui")
            Transition.Name = "Transition"
            Transition.IgnoreGuiInset = true
            Transition.DisplayOrder = 3
            local Loading = Instance.new("Frame")
            Loading.Name = "LOADING"
            Loading.Size = UDim2.fromScale(1, 1)
            Loading.BackgroundColor3 = Color3.new(0, 0, 0)
            Loading.BackgroundTransparency = 1
            Loading.Parent = Transition
            local Window = Instance.new("SurfaceGui")
            Window.Name = "Window"
            Window.Adornee = Portal
            Window.Parent = Transition
            Transition.Parent = PlayerGui
            TweenService:Create(Loading, TweenInfo.new(0.1, Enum.EasingStyle.Linear), { BackgroundTransparency = 0 }):Play()
            task.delay(0.15, Transition.Destroy, Transition)
        end)
        local Folder = Instance.new("Folder")
        Folder.Name = "Screens"
        Folder.Parent = PlayerGui
        local Hud = Instance.new("ScreenGui")
        Hud.Name = "Hud"
        Hud.Parent = Folder
        Instance.new("TextLabel").Parent = Hud
        "#,
        20,
    );
    let player_gui = instance(&scene, &["PlayerGui"]);
    assert_eq!(player_gui.class, "PlayerGui");
    let transition = instance(&scene, &["PlayerGui", "Transition"]);
    assert_eq!(transition.class, "ScreenGui");
    assert!(matches!(transition.properties.get(&rbx_dom_weak::ustr("DisplayOrder")), Some(Variant::Int32(3))));
    assert!(matches!(transition.properties.get(&rbx_dom_weak::ustr("Enabled")), Some(Variant::Bool(false))), "hidden in the file until it appears");
    instance(&scene, &["PlayerGui", "Transition", "LOADING"]);
    instance(&scene, &["PlayerGui", "Hud", "TextLabel"]);
    instance(&scene, &["Portal", "Window"]);
    assert!(scene.root().children().iter().all(|r| scene.get_by_ref(*r).unwrap().name != "Screens"), "the Folder is not recorded");
    let fade = find(&playback["tweens"], |t| t["target"] == "PlayerGui.Transition.LOADING" && t["property"] == "BackgroundTransparency");
    assert_eq!(fade.len(), 1, "{playback}");
    let keys = fade[0]["keys"].as_array().unwrap();
    let last = keys.last().unwrap();
    assert!(last["value"].as_f64() == Some(0.0) && (last["at"].as_f64().unwrap() - 0.15).abs() < 0.03, "{keys:?}");
    let enabled: Vec<(f64, bool)> =
        find(&playback["actions"], |a| a["target"] == "PlayerGui.Transition" && a["set"]["Enabled"].is_boolean()).iter().map(|a| (a["at"].as_f64().unwrap(), a["set"]["Enabled"].as_bool().unwrap())).collect();
    assert!(enabled.len() == 2 && enabled[0].1 && !enabled[1].1 && (enabled[0].0 - 0.05).abs() < 0.03 && (enabled[1].0 - 0.2).abs() < 0.03, "shown, then hidden once destroyed: {enabled:?}");
}

/// A MeshPart bound to an EditableMesh points at a snapshot of it, and a later edit of the mesh is
/// recorded as the part's MeshContent switching to a new snapshot, InitialSize keeping the size the
/// part was made at (ror scales by it).
#[test]
fn an_editable_mesh_edit_reaches_its_part() {
    let (playback, _, scene, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local Mesh = AssetService:CreateEditableMesh()
        local A = Mesh:AddVertex(Vector3.new(0, 0, 0))
        local B = Mesh:AddVertex(Vector3.new(1, 0, 0))
        local C = Mesh:AddVertex(Vector3.new(0, 1, 0))
        Mesh:AddTriangle(A, B, C)
        local Part = AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
        Part.Name = "Wave"
        Part.Parent = workspace
        task.delay(0.05, function()
            Mesh:SetPosition(B, Vector3.new(2, 0, 0))
        end)
        "#,
        8,
    );
    let wave = instance(&scene, &["Wave"]);
    let first = match wave.properties.get(&rbx_dom_weak::ustr("MeshContent")) {
        Some(Variant::ContentId(id)) => id.as_str().to_string(),
        Some(Variant::Content(content)) => format!("{content:?}"),
        other => panic!("MeshContent {other:?}"),
    };
    assert!(first.contains("file://"), "{first}");
    assert!(matches!(wave.properties.get(&rbx_dom_weak::ustr("InitialSize")), Some(Variant::Vector3(size)) if size.x == 1.0 && size.y == 1.0), "{:?}", wave.properties.get(&rbx_dom_weak::ustr("InitialSize")));
    let swaps = find(&playback["actions"], |a| a["target"] == "Wave" && a["set"]["MeshContent"].is_string());
    assert!(swaps.len() == 1 && swaps[0]["set"]["MeshContent"].as_str().unwrap().starts_with("file://") && !first.contains(swaps[0]["set"]["MeshContent"].as_str().unwrap()), "{swaps:?}");
}

/// ApplyMesh of a smaller mesh onto a part already in the world is recorded with the InitialSize it
/// gives the part, so ror scales the new mesh by Size / its own size (an effect that applies
/// a head-only mesh to a clone of a creature's body: ror kept the body's InitialSize and drew the
/// head squashed to 0.4 x 0.23 x 0.13).
#[test]
fn apply_mesh_records_the_initial_size() {
    let (playback, _, _, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local function make(size)
            local Mesh = AssetService:CreateEditableMesh()
            local A = Mesh:AddVertex(Vector3.zero)
            local B = Mesh:AddVertex(Vector3.new(size, 0, 0))
            local C = Mesh:AddVertex(Vector3.new(0, size, size))
            Mesh:AddTriangle(A, B, C)
            return AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
        end
        local Body, Head = make(8), make(2)
        Body.Name = "Body"
        Body.Parent = workspace
        task.delay(0.05, function()
            Body:ApplyMesh(Head)
            Body.Size = Head.MeshSize
        end)
        "#,
        8,
    );
    let sets = find(&playback["actions"], |a| a["target"] == "Body" && a["set"]["InitialSize"].is_array());
    assert!(sets.len() == 1 && sets[0]["set"]["InitialSize"] == serde_json::json!([2.0, 2.0, 2.0]), "{sets:?}");
}

/// A script that swaps a MeshPart's texture through TextureContent (the Content property ror reads
/// before TextureID) is recorded as a set of TextureContent, so the swap reaches the render.
#[test]
fn a_texture_content_swap_is_recorded() {
    let (playback, _, _, _) = record(
        r#"
        local Part = Instance.new("MeshPart")
        Part.Name = "Skin"
        Part.TextureContent = Content.fromUri("rbxassetid://1")
        Part.Parent = workspace
        task.delay(0.1, function()
            Part.TextureContent = Content.fromUri("rbxassetid://2")
        end)
        "#,
        12,
    );
    let swaps = find(&playback["actions"], |a| a["target"] == "Skin" && a["set"]["TextureContent"].is_string());
    let values: Vec<(f64, &str)> = swaps.iter().map(|a| (a["at"].as_f64().unwrap(), a["set"]["TextureContent"].as_str().unwrap())).collect();
    assert!(values.len() == 1 && (values[0].0 - 0.1).abs() < 0.03 && values[0].1 == "rbxassetid://2", "{values:?}");
}

/// A SurfaceAppearance AssetService builds from images loaded from assets draws those assets: its
/// maps are recorded as their ids, and an image drawn into first stays out of the file.
#[test]
fn a_surface_appearance_built_from_asset_images_is_recorded() {
    let (_, _, scene, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local Color = AssetService:CreateEditableImageAsync(Content.fromUri("rbxassetid://7"))
        local Mask = AssetService:CreateEditableImageAsync(Content.fromUri("rbxassetid://8"))
        local Drawn = AssetService:CreateEditableImageAsync(Content.fromUri("rbxassetid://9"))
        Drawn:DrawRectangle(Vector2.zero, Vector2.one, Color3.new(1, 1, 1), 0, Enum.ImageCombineType.Overwrite)
        local Surface = AssetService:CreateSurfaceAppearanceAsync({
            ColorMap = Content.fromObject(Color),
            EmissiveMask = Content.fromObject(Mask),
            NormalMap = Content.fromObject(Drawn),
        })
        Surface.EmissiveStrength = 4
        local Part = Instance.new("MeshPart")
        Part.Name = "Skin"
        Surface.Parent = Part
        Part.Parent = workspace
        assert(not pcall(function() Surface.ColorMapContent = Content.fromUri("rbxassetid://1") end), "still read-only to scripts")
        "#,
        2,
    );
    let surface = instance(&scene, &["Skin", "SurfaceAppearance"]);
    let uri = |name: &str| match surface.properties.get(&name.into()) {
        Some(Variant::Content(content)) => match content.value() {
            rbx_dom_weak::types::ContentType::Uri(uri) => Some(uri.clone()),
            _ => None,
        },
        _ => None,
    };
    assert_eq!(uri("ColorMapContent").as_deref(), Some("rbxassetid://7"));
    assert_eq!(uri("EmissiveMaskContent").as_deref(), Some("rbxassetid://8"));
    assert!(uri("NormalMapContent").is_some_and(|uri| uri.starts_with("file://")), "a drawn image gives its snapshot");
    assert!(matches!(surface.properties.get(&"EmissiveStrength".into()), Some(Variant::Float32(s)) if (s - 4.0).abs() < 1e-6));
}

/// Pixels a script writes into an image a SurfaceAppearance shows are drawn: the map is a snapshot
/// of them, and each later write plays back as the map changing to a new snapshot.
#[test]
fn written_pixels_reach_the_surface_appearance() {
    let (playback, _, scene, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local Image = AssetService:CreateEditableImage({ Size = Vector2.new(2, 1) })
        local function Fill(R, G, B)
            local Pixels = buffer.create(8)
            for Index = 0, 1 do
                buffer.writeu8(Pixels, Index * 4, R)
                buffer.writeu8(Pixels, Index * 4 + 1, G)
                buffer.writeu8(Pixels, Index * 4 + 2, B)
                buffer.writeu8(Pixels, Index * 4 + 3, 255)
            end
            Image:WritePixelsBuffer(Vector2.zero, Image.Size, Pixels)
        end
        Fill(10, 20, 30)
        local Surface = AssetService:CreateSurfaceAppearanceAsync({ ColorMap = Content.fromObject(Image) })
        local Part = Instance.new("MeshPart")
        Part.Name = "Skin"
        Surface.Parent = Part
        Part.Parent = workspace
        local Read = Image:ReadPixelsBuffer(Vector2.new(1, 0), Vector2.new(1, 1))
        assert(buffer.readu8(Read, 2) == 30, "reads back what was written")
        task.delay(0.1, function()
            Fill(200, 100, 50)
        end)
        "#,
        12,
    );
    let read = |uri: &str| crate::images::decode(&std::fs::read(uri.strip_prefix("file://").unwrap()).unwrap()).unwrap();
    let surface = instance(&scene, &["Skin", "SurfaceAppearance"]);
    let first = match surface.properties.get(&"ColorMapContent".into()) {
        Some(Variant::Content(content)) => match content.value() {
            rbx_dom_weak::types::ContentType::Uri(uri) => uri.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(&read(&first).rgba[..4], &[10, 20, 30, 255]);
    let sets = find(&playback["actions"], |a| a["target"] == "Skin.SurfaceAppearance" && a["set"]["ColorMapContent"].is_string());
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert!((sets[0]["at"].as_f64().unwrap() - 0.1).abs() < 0.03);
    assert_eq!(&read(sets[0]["set"]["ColorMapContent"].as_str().unwrap()).rgba[4..8], &[200, 100, 50, 255]);
}

/// A part's SurfaceAppearance swapped mid-run plays back as the swap: the new one stays out of the
/// world until it is parented in, the old one leaves, and an emissive ramp is recorded.
#[test]
fn a_surface_appearance_swap_is_recorded() {
    let (playback, _, _, _) = record(
        r#"
        local Part = Instance.new("MeshPart")
        Part.Name = "Skin"
        local Old = Instance.new("SurfaceAppearance")
        Old.Name = "Old"
        Old.Parent = Part
        Part.Parent = workspace
        task.delay(0.1, function()
            Old.Parent = nil
            local New = Instance.new("SurfaceAppearance")
            New.Name = "New"
            New.Parent = Part
            for Step = 1, 3 do
                task.wait()
                New.EmissiveStrength = Step * 2
            end
        end)
        "#,
        16,
    );
    let parents = |target: &str| -> Vec<(f64, String)> {
        find(&playback["actions"], |a| a["target"] == target && a["parent"].is_string())
            .iter()
            .map(|a| (a["at"].as_f64().unwrap(), a["parent"].as_str().unwrap().to_string()))
            .collect()
    };
    let old = parents("Skin.Old");
    assert!(old.len() == 1 && (old[0].0 - 0.1).abs() < 0.03 && old[0].1.is_empty(), "the old one leaves: {old:?}");
    let new = parents("Skin.New");
    assert!(new.len() == 2 && new[0] == (0.0, String::new()) && (new[1].0 - 0.1).abs() < 0.03 && new[1].1 == "Skin", "out until parented in: {new:?}");
    let strength = find(&playback["tweens"], |t| t["target"] == "Skin.New" && t["property"] == "EmissiveStrength").len()
        + find(&playback["actions"], |a| a["target"] == "Skin.New" && a["set"]["EmissiveStrength"].is_number()).len();
    assert!(strength > 0, "the emissive ramp is recorded");
}

/// DrawImage and DrawRectangle combine as Roblox's ImageCombineType says: a crossfade of two images
/// (the second drawn over the first after its alpha was cut with a Subtract rectangle) lands between them.
#[test]
fn draw_operations_combine_pixels() {
    let (_, _, _, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local Size = Vector2.new(2, 2)
        local function Filled(R, G, B)
            local Image = AssetService:CreateEditableImage({ Size = Size })
            Image:DrawRectangle(Vector2.zero, Size, Color3.fromRGB(R, G, B), 0, Enum.ImageCombineType.Overwrite)
            return Image
        end
        local A, B, Out = Filled(0, 0, 0), Filled(200, 100, 0), Filled(9, 9, 9)
        local Tmp = AssetService:CreateEditableImage({ Size = Size })
        Out:DrawImage(Vector2.zero, A, Enum.ImageCombineType.Overwrite)
        Tmp:DrawImage(Vector2.zero, B, Enum.ImageCombineType.Overwrite)
        Tmp:DrawRectangle(Vector2.zero, Size, Color3.new(0, 0, 0), 0.75, Enum.ImageCombineType.Subtract)
        Out:DrawImage(Vector2.zero, Tmp, Enum.ImageCombineType.BlendSourceOver)
        local Pixel = Out:ReadPixelsBuffer(Vector2.new(1, 1), Vector2.one)
        local R, G, Alpha = buffer.readu8(Pixel, 0), buffer.readu8(Pixel, 1), buffer.readu8(Pixel, 3)
        assert(math.abs(R - 150) <= 1 and math.abs(G - 75) <= 1 and Alpha == 255, `crossfade 0.75: {R} {G} {Alpha}`)
        "#,
        1,
    );
}

/// A Decal's UVOffset and UVScale play back: a sheet flipbook on a flat part steps UVOffset across its
/// cells (a baked sheet), where a recording without them drew the sheet's first cell throughout.
#[test]
fn a_decals_uv_offset_and_scale_play_back() {
    let (playback, _, scene, _) = record(
        r#"
        local Part = Instance.new("Part")
        Part.Name = "Plate"
        local Decal = Instance.new("Decal")
        Decal.UVScale = Vector2.new(4, 4)
        Decal.Parent = Part
        Part.Parent = workspace
        task.delay(0.1, function()
            Decal.UVOffset = Vector2.new(2, 1)
        end)
        "#,
        16,
    );
    match instance(&scene, &["Plate", "Decal"]).properties.get(&"UVScale".into()) {
        Some(Variant::Vector2(scale)) => assert_eq!((scale.x, scale.y), (4.0, 4.0)),
        other => panic!("UVScale: {other:?}"),
    }
    let sets = find(&playback["actions"], |a| a["target"] == "Plate.Decal" && a["set"]["UVOffset"].is_array());
    assert!(sets.len() == 1 && (sets[0]["at"].as_f64().unwrap() - 0.1).abs() < 0.03, "{sets:?}");
    let offset = &sets[0]["set"]["UVOffset"];
    assert_eq!((offset[0].as_f64(), offset[1].as_f64()), (Some(2.0), Some(1.0)), "{offset}");
}

/// An EditableImage shown through other Content properties that take images (a MeshPart's
/// TextureContent, a Decal's ColorMapContent, an ImageLabel's ImageContent) is drawn as a snapshot of
/// its pixels, each later write plays back as the property changing, and an image assigned before
/// it is drawn into, or from an asset, shows what it holds then.
#[test]
fn an_editable_image_reaches_every_image_content_property() {
    let (playback, _, scene, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local Image = AssetService:CreateEditableImage({ Size = Vector2.new(4, 4) })
        -- an Object, not an Instance: ClassName and IsA as Roblox's Object has them
        assert(Image.ClassName == "EditableImage" and Image:IsA("EditableImage") and Image:IsA("Object") and not Image:IsA("Instance"))
        Image:DrawRectangle(Vector2.zero, Image.Size, Color3.fromRGB(10, 20, 30), 0, Enum.ImageCombineType.Overwrite)
        local Part = Instance.new("MeshPart")
        Part.Name = "Skin"
        Part.TextureContent = Content.fromObject(Image)
        local Decal = Instance.new("Decal")
        Decal.ColorMapContent = Content.fromObject(Image)
        Decal.Parent = Part
        local Asset = AssetService:CreateEditableImageAsync(Content.fromUri("rbxassetid://7"))
        local Gui = Instance.new("SurfaceGui")
        local Label = Instance.new("ImageLabel")
        Label.ImageContent = Content.fromObject(Asset)
        Label.Parent = Gui
        Gui.Parent = Part
        Part.Parent = workspace
        assert(Part.TextureContent.SourceType == Enum.ContentSourceType.Uri, "holds the snapshot")
        task.delay(0.1, function()
            Image:DrawCircle(Vector2.new(2, 2), 3, Color3.fromRGB(250, 0, 0), 0, Enum.ImageCombineType.Overwrite)
        end)
        task.delay(0.15, function()
            Part.TextureContent = Content.fromUri("rbxassetid://2")
        end)
        task.delay(0.2, function()
            Image:DrawRectangle(Vector2.zero, Image.Size, Color3.fromRGB(0, 0, 250), 0, Enum.ImageCombineType.Overwrite)
        end)
        "#,
        16,
    );
    let read = |uri: &str| crate::images::decode(&std::fs::read(uri.strip_prefix("file://").unwrap()).unwrap()).unwrap();
    let uri = |path: &[&str], name: &str| match instance(&scene, path).properties.get(&name.into()) {
        Some(Variant::Content(content)) => match content.value() {
            rbx_dom_weak::types::ContentType::Uri(uri) => uri.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{name}: {other:?}"),
    };
    let first = uri(&["Skin"], "TextureContent");
    assert!(first.starts_with("file://"), "{first}");
    assert_eq!(&read(&first).rgba[..4], &[10, 20, 30, 255]);
    // (ColorMapContent is a view of the Decal's Texture, recorded as TextureContent and Texture)
    assert_eq!(uri(&["Skin", "Decal"], "TextureContent"), first, "one snapshot for both users");
    assert_eq!(uri(&["Skin", "SurfaceGui", "ImageLabel"], "ImageContent"), "rbxassetid://7", "an unwritten asset image draws its asset");
    // the circle's write at 0.1 s: both users take the new snapshot; the part's swap at 0.15 s leaves the image
    let part_sets = find(&playback["actions"], |a| a["target"] == "Skin" && a["set"]["TextureContent"].is_string());
    let decal_sets = find(&playback["actions"], |a| a["target"] == "Skin.Decal" && a["set"]["TextureContent"].is_string());
    let at = |a: &Json| a["at"].as_f64().unwrap();
    assert!(part_sets.len() == 2 && (at(part_sets[0]) - 0.1).abs() < 0.03, "{part_sets:?}");
    assert_eq!(part_sets[1]["set"]["TextureContent"], "rbxassetid://2");
    assert!(decal_sets.len() == 2 && (at(decal_sets[1]) - 0.2).abs() < 0.03, "{decal_sets:?}");
    let circle = read(decal_sets[0]["set"]["TextureContent"].as_str().unwrap());
    assert_eq!(&circle.rgba[(2 * 4 + 2) * 4..(2 * 4 + 2) * 4 + 4], &[250, 0, 0, 255], "the circle's middle");
    assert_eq!(&read(decal_sets[1]["set"]["TextureContent"].as_str().unwrap()).rgba[..4], &[0, 0, 250, 255]);
}

/// DrawCircle and DrawLine draw (anti-aliased unless AntiAliasing is Disabled) and DrawImageTransformed
/// places a scaled, turned image by its pivot.
#[test]
fn draw_circle_line_and_transformed_images() {
    let (_, _, _, _) = record(
        r#"
        local AssetService = game:GetService("AssetService")
        local function Pixel(Image, X, Y)
            local P = Image:ReadPixelsBuffer(Vector2.new(X, Y), Vector2.one)
            return buffer.readu8(P, 0), buffer.readu8(P, 1), buffer.readu8(P, 2), buffer.readu8(P, 3)
        end
        local Image = AssetService:CreateEditableImage({ Size = Vector2.new(16, 16) })
        Image:DrawCircle(Vector2.new(8, 8), 4, Color3.new(1, 1, 1), 0, Enum.ImageCombineType.Overwrite)
        local R, _, _, A = Pixel(Image, 8, 8)
        assert(R == 255 and A == 255, `inside: {R} {A}`)
        -- (pixel 11, 10's centre lies 4.3 from the circle's: a fifth covered)
        local _, _, _, Edge = Pixel(Image, 11, 10)
        assert(Edge > 0 and Edge < 255, `a smoothed edge: {Edge}`)
        local _, _, _, Out = Pixel(Image, 14, 8)
        assert(Out == 0, `outside: {Out}`)
        local Hard = AssetService:CreateEditableImage({ Size = Vector2.new(16, 16) })
        Hard:DrawCircle(Vector2.new(8, 8), 4, Color3.new(1, 1, 1), 0, Enum.ImageCombineType.Overwrite, Enum.AntiAliasing.Disabled)
        local _, _, _, HardEdge = Pixel(Hard, 11, 10)
        assert(HardEdge == 0, `a hard edge: {HardEdge}`)
        local Line = AssetService:CreateEditableImage({ Size = Vector2.new(16, 16) })
        Line:DrawLine(Vector2.new(0, 5), Vector2.new(15, 5), Color3.new(0, 1, 0), 0, Enum.ImageCombineType.Overwrite)
        local _, G, _, LineA = Pixel(Line, 7, 5)
        assert(G == 255 and LineA == 255, `on the line: {G} {LineA}`)
        local _, _, _, Off = Pixel(Line, 7, 8)
        assert(Off == 0, `off the line: {Off}`)
        local Source = AssetService:CreateEditableImage({ Size = Vector2.new(2, 2) })
        Source:DrawRectangle(Vector2.zero, Vector2.new(2, 2), Color3.new(1, 0, 0), 0, Enum.ImageCombineType.Overwrite)
        local Target = AssetService:CreateEditableImage({ Size = Vector2.new(16, 16) })
        Target:DrawImageTransformed(Vector2.new(8, 8), Vector2.new(4, 4), 45, Source, { SamplingMode = Enum.ResamplerMode.Pixelated })
        local TR, _, _, TA = Pixel(Target, 8, 8)
        assert(TR == 255 and TA == 255, `the pivot lands at the position: {TR} {TA}`)
        local _, _, _, Corner = Pixel(Target, 4, 4)
        assert(Corner == 0, `a corner a 45 degree turn leaves empty: {Corner}`)
        local _, _, _, Tip = Pixel(Target, 8, 3)
        assert(Tip == 255, `the turned square's tip: {Tip}`)
        "#,
        1,
    );
}

/// A class rbx-dom's database does not know (UIShadow) is written with the properties Studio's dump
/// serializes for it and its name, without the attributes and tags rbx-binary cannot write for it.
#[test]
fn a_class_rbx_dom_does_not_know_is_recorded() {
    let (_, _, scene, _) = record(
        r#"
        local Gui = Instance.new("BillboardGui")
        Gui.Name = "Bubble"
        local Plate = Instance.new("Frame")
        Plate.Name = "Plate"
        Plate.Parent = Gui
        local Shadow = Instance.new("UIShadow")
        Shadow.Name = "Glow"
        Shadow.Transparency = 0.55
        Shadow:SetAttribute("Kind", "glow")
        Shadow:AddTag("Glow")
        Shadow.Parent = Plate
        Gui.Parent = workspace
        "#,
        4,
    );
    let shadow = instance(&scene, &["Bubble", "Plate", "Glow"]);
    assert_eq!(shadow.class, "UIShadow");
    assert!(!shadow.properties.contains_key(&"Name".into()), "the name is the instance's, not a property");
    assert!(matches!(shadow.properties.get(&"Transparency".into()), Some(Variant::Float32(t)) if (t - 0.55).abs() < 1e-6));
    assert!(!shadow.properties.contains_key(&"Tags".into()) && !shadow.properties.contains_key(&"Attributes".into()));
}

/// A line typed out through MaxVisibleGraphemes and a glow fading out are recorded as sets.
#[test]
fn typed_text_and_a_fading_glow_are_recorded() {
    let (playback, _, _, _) = record(
        r#"
        local Gui = Instance.new("BillboardGui")
        Gui.Name = "Bubble"
        local Line = Instance.new("TextLabel")
        Line.Name = "Line"
        Line.Text = "Hello"
        Line.MaxVisibleGraphemes = 0
        Line.Parent = Gui
        local Shadow = Instance.new("UIShadow")
        Shadow.Parent = Line
        Gui.Parent = workspace
        task.delay(0.1, function()
            Line.MaxVisibleGraphemes = 3
            Shadow.Transparency = 1
        end)
        "#,
        12,
    );
    let typed = find(&playback["actions"], |a| a["target"] == "Bubble.Line" && a["set"]["MaxVisibleGraphemes"].as_f64() == Some(3.0));
    assert_eq!(typed.len(), 1, "{}", playback["actions"]);
    let faded = find(&playback["actions"], |a| a["target"] == "Bubble.Line.UIShadow" && a["set"]["Transparency"] == 1.0);
    assert_eq!(faded.len(), 1, "{}", playback["actions"]);
}

/// Clones keep their template's UniqueId; the scene gives a repeated one a replacement made from it
/// (rbx-dom would draw a random one), so recording the same run twice writes the same bytes.
#[test]
fn a_scene_of_clones_is_the_same_bytes_each_time() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::UniqueId};
    let scene = || {
        let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
        let lua = Lua::new();
        engine_lua::start(&lua, &engine).expect("start");
        let mut model = WeakDom::new(InstanceBuilder::new("DataModel"));
        let template = InstanceBuilder::new("Part").with_name("Template").with_property("UniqueId", Variant::UniqueId(UniqueId::new(7, 1000, 99)));
        let root = model.root_ref();
        model.insert(root, template);
        let storage = crate::services::get_service(&engine, engine.world().game, "ReplicatedStorage").unwrap();
        crate::project::convert_instances(&engine, &model, model.root().children(), Some(storage));
        let recorder = Recorder::install(&engine, 60.0);
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            crate::instance::create_named(&mut state, &engine.reflection, class, "Harness")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let source = r#"for Index = 1, 3 do local Copy = game.ReplicatedStorage.Template:Clone() Copy.Name = "Copy" .. Index Copy.Parent = workspace end"#;
        let function = lua.load(source).set_name("=Harness").set_environment(env).into_function().expect("compile");
        engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        recorder.sample(&engine, 0);
        let out = std::env::temp_dir().join(format!("ree-scene-bytes-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        recorder.write(&engine, &out, Map::new()).expect("write");
        let bytes = std::fs::read(out.join("scene.rbxm")).unwrap();
        let _ = std::fs::remove_dir_all(&out);
        drop(lua);
        engine.teardown();
        bytes
    };
    let first = scene();
    assert_eq!(first, scene(), "the same scene bytes");
    let dom = rbx_binary::from_reader(first.as_slice()).expect("scene.rbxm");
    let ids: std::collections::HashSet<String> = dom.root().children().iter().map(|r| format!("{:?}", dom.get_by_ref(*r).unwrap().properties.get(&"UniqueId".into()))).collect();
    assert_eq!(ids.len(), 3, "each copy has its own UniqueId: {ids:?}");
}

/// The recorder walks the world only after something was parented (the dom's structure version): a
/// run that moves, adds, reparents, destroys and brings back instances among frames where nothing is
/// parented records what walking every frame records.
#[test]
fn skipping_the_walk_in_frames_nothing_was_parented_records_the_same() {
    let source = r#"
        local Lighting = game:GetService("Lighting")
        local RunService = game:GetService("RunService")
        local Mover = Instance.new("Part")
        Mover.Name = "Mover"
        Mover.Anchored = true
        Mover.Parent = workspace
        local Elapsed = 0
        RunService.Heartbeat:Connect(function(Step)
            Elapsed += Step
            Mover.CFrame = CFrame.new(Elapsed * 10, 2, 0)
            Mover.Transparency = math.min(Elapsed, 0.9)
        end)
        local Holder = Instance.new("Folder")
        Holder.Name = "Holder"
        local Other = Instance.new("Folder")
        Other.Name = "Other"
        Other.Parent = workspace
        local Kept = Instance.new("Part")
        Kept.Name = "Kept"
        Kept.Anchored = true
        Kept.Parent = Holder
        local Emitter = Instance.new("ParticleEmitter")
        Emitter.Parent = Kept
        task.delay(0.1, function() Holder.Parent = workspace end)
        task.delay(0.15, function()
            local Light = Instance.new("ColorCorrectionEffect")
            Light.Parent = Lighting
        end)
        task.delay(0.2, function() Kept.Parent = Other Emitter:Emit(5) end)
        task.delay(0.25, function() Holder:Destroy() end)
        task.delay(0.3, function() Kept.Parent = nil end)
        task.delay(0.35, function() Kept.Parent = workspace Kept.Color = Color3.new(1, 0, 0) end)
    "#;
    let skipping = record_with(source, 40, false);
    let walking = record_with(source, 40, true);
    assert_eq!(skipping.0, walking.0, "playback");
    assert_eq!(skipping.1, walking.1, "events");
    assert_eq!(skipping.3, walking.3, "summary");
    assert_eq!(skipping.4, walking.4, "scene.rbxm");
    assert!(skipping.0.to_string().contains("Kept"), "{}", skipping.0);
}

/// `--frame-times`: the listed moments up to --seconds, then one every 1 / --fps.
#[test]
fn frame_times_replay_a_take_then_go_on_at_the_frame_rate() {
    let args = |frame_times: Option<Vec<f64>>| super::run::RecordArgs {
        engine: crate::launch::Args::default(),
        harness: String::new(),
        out: None,
        seconds: 0.1,
        fps: 60.0,
        context: None,
        model_assets: Default::default(),
        animations: Vec::new(),
        frame_times,
        script_timeout: None,
        script_timeouts: None,
        timing_check: false,
        clock_probe: None,
    };
    let round = |times: Vec<f64>| times.into_iter().map(|t| (t * 1e4).round() / 1e4).collect::<Vec<_>>();
    assert_eq!(round(args(Some(vec![0.03, 0.05])).frame_schedule()), vec![0.03, 0.05, 0.0667, 0.0833, 0.1]);
    assert_eq!(round(args(Some(vec![0.04, 0.2])).frame_schedule()), vec![0.04, 0.0567, 0.0733, 0.09]);
    assert_eq!(args(None).frame_schedule().len(), 6);
}

#[test]
fn a_part_physics_moves_marks_where_its_motion_turns_physical() {
    let (playback, _, _, _) = record(
        r#"
        local Falling = Instance.new("Part")
        Falling.Name = "Falling"
        Falling.Anchored = true
        Falling.CanCollide = false
        Falling.CFrame = CFrame.new(0, 100, 0)
        Falling.Parent = workspace
        local Moved = Instance.new("Part")
        Moved.Name = "Moved"
        Moved.Anchored = true
        Moved.Parent = workspace
        task.delay(0.1, function()
            Falling.CFrame = CFrame.new(5, 100, 0)
            Moved.CFrame = CFrame.new(0, 10, 0)
        end)
        task.delay(0.25, function()
            Falling.Anchored = false
        end)
        local Steered = Instance.new("Part")
        Steered.Name = "Steered"
        Steered.CanCollide = false
        Steered.CFrame = CFrame.new(20, 100, 0)
        Steered.Parent = workspace
        task.delay(0.3, function()
            Steered.CFrame = CFrame.new(20, 100, 0)
        end)
        "#,
        36,
    );
    let falling = find(&playback["motion"], |m| m["target"] == "Falling");
    assert_eq!(falling.len(), 1, "{}", playback["motion"]);
    let from = falling[0]["physicsFrom"].as_f64().expect("no physicsFrom");
    assert!((0.24..0.32).contains(&from), "physics from {from}");
    let last = falling[0]["keys"].as_array().unwrap().last().unwrap();
    assert!(last["cframe"][1].as_f64().unwrap() < 99.0, "it did not fall: {last}");
    let moved = find(&playback["motion"], |m| m["target"] == "Moved");
    assert!(moved[0].get("physicsFrom").is_none(), "a scripted move marked physical");
    // a falling part a script moves too: its locked particles go with the script's moves
    let steered = find(&playback["motion"], |m| m["target"] == "Steered");
    assert_eq!(steered.len(), 1, "{}", playback["motion"]);
    assert!(steered[0].get("physicsFrom").is_none(), "a steered fall marked physical: {}", steered[0]);
}

/// `Emit` spawns its particles at once, where the emitter is at the call: an attachment moved to four
/// corners in one frame, emitting after each move, leaves four bursts at four corners (an effect that
/// locks its rectangle does so), and a part resized and moved between two emits leaves each
/// burst over the part as it was then. The frame's motion holds only the last place: each emit carries
/// its own.
#[test]
fn each_emit_leaves_from_where_its_emitter_was_at_the_call() {
    let (playback, _, _, summary) = record(
        r#"
        local Plate = Instance.new("Part")
        Plate.Name = "Plate"
        Plate.Anchored = true
        Plate.Transparency = 1
        Plate.CFrame = CFrame.new(0, 2, 0)
        Plate.Parent = workspace
        local Corner = Instance.new("Attachment")
        Corner.Name = "Corner"
        Corner.Parent = Plate
        local Burst = Instance.new("ParticleEmitter")
        Burst.Name = "Burst"
        Burst.Rate = 0
        Burst.Parent = Corner
        local Field = Instance.new("Part")
        Field.Name = "Field"
        Field.Anchored = true
        Field.Transparency = 1
        Field.Parent = workspace
        local Lift = Instance.new("ParticleEmitter")
        Lift.Name = "Lift"
        Lift.Rate = 0
        Lift.Parent = Field
        task.delay(0.1, function()
            for _, corner in { Vector3.new(-6, 0, -4), Vector3.new(6, 0, -4), Vector3.new(6, 0, 4), Vector3.new(-6, 0, 4) } do
                Corner.WorldPosition = corner
                Burst:Emit(8)
            end
            Field.Size = Vector3.new(12, 0.1, 8)
            Field.CFrame = CFrame.new(0, 0.2, 0)
            Lift:Emit(3)
            Field.Size = Vector3.new(4, 0.1, 4)
            Field.CFrame = CFrame.new(10, 0.2, 0) * CFrame.Angles(0, math.pi / 2, 0)
            Lift:Emit(5)
        end)
        "#,
        12,
    );
    let bursts = find(&playback["actions"], |a| a["target"] == "Plate.Corner.Burst" && a["emit"].is_number());
    assert_eq!(bursts.len(), 4, "{}", playback["actions"]);
    let at = bursts[0]["at"].as_f64().unwrap();
    assert!((at - 0.1).abs() < 0.02, "emitted at {at}");
    let corners: Vec<[f64; 3]> = bursts
        .iter()
        .map(|burst| {
            assert_eq!(burst["at"].as_f64(), Some(at), "the four bursts are one frame's");
            assert_eq!(burst["emit"], 8);
            assert!(burst.get("partSize").is_none(), "an attachment has no size: {burst}");
            let cframe = burst["cframe"].as_array().unwrap_or_else(|| panic!("no cframe: {burst}"));
            assert_eq!(cframe.len(), 12);
            let rotation: Vec<f64> = cframe[3..].iter().map(|v| v.as_f64().unwrap()).collect();
            assert_eq!(rotation, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
            [cframe[0].as_f64().unwrap(), cframe[1].as_f64().unwrap(), cframe[2].as_f64().unwrap()]
        })
        .collect();
    assert_eq!(corners, [[-6.0, 0.0, -4.0], [6.0, 0.0, -4.0], [6.0, 0.0, 4.0], [-6.0, 0.0, 4.0]]);
    // the attachment's own track holds the frame's last place only
    let corner = find(&playback["motion"], |m| m["target"] == "Plate.Corner");
    let last = corner[0]["keys"].as_array().unwrap().last().unwrap()["cframe"].clone();
    assert_eq!((last[0].as_f64(), last[2].as_f64()), (Some(-6.0), Some(4.0)), "{last}");
    // the part's emitter: each burst over the part as it was at its call
    let lifts = find(&playback["actions"], |a| a["target"] == "Field.Lift" && a["emit"].is_number());
    assert_eq!(lifts.len(), 2, "{}", playback["actions"]);
    assert_eq!(lifts[0]["emit"], 3);
    assert_eq!(lifts[0]["partSize"], serde_json::json!([12.0, 0.1, 8.0]));
    assert_eq!(lifts[0]["cframe"], serde_json::json!([0.0, 0.2, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
    assert_eq!(lifts[1]["emit"], 5);
    assert_eq!(lifts[1]["partSize"], serde_json::json!([4.0, 0.1, 4.0]));
    let turned = lifts[1]["cframe"].as_array().unwrap();
    assert_eq!((turned[0].as_f64(), turned[5].as_f64(), turned[9].as_f64()), (Some(10.0), Some(1.0), Some(-1.0)), "{}", lifts[1]);
    assert_eq!(summary["emits"], 6);
}

/// An instance reference written after the instance appeared plays as a `set` naming the referenced
/// instance's path (none: null): a beam parented first and given its attachments later, then one of
/// them taken away; a trail given its ends a frame after it appeared; a model given its PrimaryPart.
/// References written before the frame ends are in the file, as before.
#[test]
fn references_written_after_an_instance_appears_play_back() {
    let (playback, _, scene, _) = record(
        r#"
        local Holder = Instance.new("Part")
        Holder.Name = "Holder"
        Holder.Anchored = true
        Holder.Transparency = 1
        Holder.Parent = workspace
        local A = Instance.new("Attachment")
        A.Name = "A"
        A.Position = Vector3.new(-4, 0, 0)
        A.Parent = Holder
        local B = Instance.new("Attachment")
        B.Name = "B"
        B.Position = Vector3.new(4, 0, 0)
        B.Parent = Holder
        -- given its ends in the frame it appears: the file holds them
        local Early = Instance.new("Beam")
        Early.Name = "Early"
        Early.Parent = Holder
        Early.Attachment0 = A
        Early.Attachment1 = B
        local Beam = Instance.new("Beam")
        Beam.Name = "Beam"
        Beam.Parent = Holder
        local Trail = Instance.new("Trail")
        Trail.Name = "Trail"
        Trail.Parent = Holder
        local Model = Instance.new("Model")
        Model.Name = "Rig"
        local Root = Instance.new("Part")
        Root.Name = "Root"
        Root.Anchored = true
        Root.Parent = Model
        Model.Parent = workspace
        -- an attachment that comes into the world later
        local Late = Instance.new("Attachment")
        Late.Name = "Late"
        task.delay(0.05, function()
            Beam.Attachment0 = A
            Beam.Attachment1 = B
            Trail.Attachment0 = A
            Trail.Attachment1 = Late
            Model.PrimaryPart = Root
        end)
        task.delay(0.1, function()
            Late.Parent = Holder
        end)
        task.delay(0.2, function()
            Beam.Attachment1 = nil
            Beam.Attachment1 = B
            Beam.Attachment1 = nil
        end)
        "#,
        20,
    );
    let sets = |target: &str, property: &str| -> Vec<(f64, Json)> {
        find(&playback["actions"], |a| a["target"] == target && a["set"].get(property).is_some()).iter().map(|a| (a["at"].as_f64().unwrap(), a["set"][property].clone())).collect()
    };
    let attachment0 = sets("Holder.Beam", "Attachment0");
    assert_eq!(attachment0.len(), 1, "{}", playback["actions"]);
    assert!((attachment0[0].0 - 0.05).abs() < 0.02 && attachment0[0].1 == "Holder.A", "{attachment0:?}");
    let attachment1 = sets("Holder.Beam", "Attachment1");
    assert_eq!(attachment1.len(), 2, "{attachment1:?}");
    assert_eq!(attachment1[0].1, "Holder.B");
    // taken away, back and away again in one frame: the frame's last
    assert!((attachment1[1].0 - 0.2).abs() < 0.02 && attachment1[1].1.is_null(), "{attachment1:?}");
    assert_eq!(sets("Holder.Trail", "Attachment0")[0].1, "Holder.A");
    // named before it was in the world: its path, which the file has from where it appeared
    assert_eq!(sets("Holder.Trail", "Attachment1")[0].1, "Holder.Late");
    assert_eq!(sets("Rig", "PrimaryPart")[0].1, "Rig.Root");
    assert!(sets("Holder.Early", "Attachment0").is_empty() && sets("Holder.Early", "Attachment1").is_empty(), "{}", playback["actions"]);
    let early = instance(&scene, &["Holder", "Early"]);
    let a = instance(&scene, &["Holder", "A"]).referent();
    assert_eq!(early.properties.get(&rbx_dom_weak::ustr("Attachment0")), Some(&Variant::Ref(a)));
    // the later beam's file has none
    let beam = instance(&scene, &["Holder", "Beam"]);
    assert!(beam.properties.get(&rbx_dom_weak::ustr("Attachment0")).is_none_or(|value| *value == Variant::Ref(rbx_dom_weak::types::Ref::none())));
}

/// Context.TraceWrites reports the Luau line behind a Parent write and a listed property write, whether the
/// write entered or left the Workspace, and nothing for a write the engine made (Debris).
#[test]
fn trace_writes_names_the_line_of_each_write() {
    let root = std::env::temp_dir().join(format!("ree-trace-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = root.join("Trace.luau");
    std::fs::write(
        &harness,
        "return function(Context)\n\
         local Drain = Context.TraceWrites({'Size'}, 2)\n\
         local Part = Instance.new('Part')\n\
         local Model = Instance.new('Model')\n\
         Part.Parent = Model\n\
         Model.Parent = workspace\n\
         Part.Size = Vector3.new(3, 3, 3)\n\
         Part.Transparency = 0.5\n\
         Model.Parent = nil\n\
         local Out = {}\n\
         for _, W in Drain() do\n\
         table.insert(Out, W.Kind .. ' ' .. W.Instance.Name .. ' ' .. tostring(W.Add) .. ' ' .. tostring(W.Remove) .. ' ' .. (W.Property or '-') .. ' ' .. (W.Stack[1] or 'none'))\n\
         end\n\
         print(table.concat(Out, '|'))\n\
         end",
    )
    .unwrap();
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let emulator = crate::Emulator::start(engine, &crate::mount::ProjectSpec::default()).expect("emulator");
    let args = super::run::RecordArgs {
        engine: crate::launch::Args::default(),
        harness: harness.to_string_lossy().into_owned(),
        out: None,
        seconds: 1.0 / 60.0,
        fps: 60.0,
        context: None,
        model_assets: Default::default(),
        animations: Vec::new(),
        frame_times: None,
        script_timeout: None,
        script_timeouts: None,
        timing_check: false,
        clock_probe: None,
    };
    let ran = super::run::run(&emulator.lua, &emulator.engine, &args, None).expect("run");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(ran.status, "ok", "{:?}", ran.failure);
    let lines: Vec<String> = emulator.output().iter().map(|line| line.message.clone()).collect();
    // (Part into Model and Model into Workspace and out are Parent writes; only the Size write is listed;
    // each names its line, 5 7 6 9 of the harness, and Add / Remove tell the world's)
    let entries: Vec<&str> = lines.last().expect("output").split('|').collect();
    assert_eq!(entries.len(), 4, "{lines:?}");
    assert!(entries[0].starts_with("parent Part false false - ") && entries[0].ends_with(":5"), "{entries:?}");
    assert!(entries[1].starts_with("parent Model true false - ") && entries[1].ends_with(":6"), "{entries:?}");
    assert!(entries[2].starts_with("property Part nil nil Size ") && entries[2].ends_with(":7"), "{entries:?}");
    assert!(entries[3].starts_with("parent Model false true - ") && entries[3].ends_with(":9"), "{entries:?}");
}
/// Context.TraceWrites({'*'}) reports every property write that changes a value, and SetAttribute, with the line of
/// each: CastShadow, a Motor6D's C0, a Color and an attribute; a write of the value a property already holds and an
/// attribute set to what it was report nothing.
#[test]
fn trace_writes_star_names_every_property_and_attribute_write() {
    let root = std::env::temp_dir().join(format!("ree-trace-star-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = root.join("TraceStar.luau");
    std::fs::write(
        &harness,
        "return function(Context)\n\
         local Drain = Context.TraceWrites({'*'}, 2)\n\
         local Part = Instance.new('Part')\n\
         local Joint = Instance.new('Motor6D')\n\
         Drain()\n\
         Part.CastShadow = false\n\
         Part.CastShadow = false\n\
         Joint.C0 = CFrame.new(1, 2, 3)\n\
         Part.Color = Color3.new(1, 0, 0)\n\
         Part:SetAttribute('Tier', 2)\n\
         Part:SetAttribute('Tier', 2)\n\
         Part:SetAttribute('Tier', nil)\n\
         local Out = {}\n\
         for _, W in Drain() do\n\
         table.insert(Out, W.Kind .. ' ' .. W.Instance.ClassName .. ' ' .. (W.Property or '-') .. ' ' .. (W.Stack[1] or 'none'))\n\
         end\n\
         print(table.concat(Out, '|'))\n\
         end",
    )
    .unwrap();
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let emulator = crate::Emulator::start(engine, &crate::mount::ProjectSpec::default()).expect("emulator");
    let args = super::run::RecordArgs {
        engine: crate::launch::Args::default(),
        harness: harness.to_string_lossy().into_owned(),
        out: None,
        seconds: 1.0 / 60.0,
        fps: 60.0,
        context: None,
        model_assets: Default::default(),
        animations: Vec::new(),
        frame_times: None,
        script_timeout: None,
        script_timeouts: None,
        timing_check: false,
        clock_probe: None,
    };
    let ran = super::run::run(&emulator.lua, &emulator.engine, &args, None).expect("run");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(ran.status, "ok", "{:?}", ran.failure);
    let lines: Vec<String> = emulator.output().iter().map(|line| line.message.clone()).collect();
    // (the repeated CastShadow and the repeated attribute change nothing; lines 6, 8, 9, 10 and 12 of the harness)
    let entries: Vec<&str> = lines.last().expect("output").split('|').collect();
    assert_eq!(entries.len(), 5, "{lines:?}");
    assert!(entries[0].starts_with("property Part CastShadow ") && entries[0].ends_with(":6"), "{entries:?}");
    assert!(entries[1].starts_with("property Motor6D C0 ") && entries[1].ends_with(":8"), "{entries:?}");
    assert!(entries[2].starts_with("property Part Color ") && entries[2].ends_with(":9"), "{entries:?}");
    assert!(entries[3].starts_with("attribute Part Tier ") && entries[3].ends_with(":10"), "{entries:?}");
    assert!(entries[4].starts_with("attribute Part Tier ") && entries[4].ends_with(":12"), "{entries:?}");
}
/// Context.ProfilePhase names the phase of the allocation profile from there on: the harness's setup stays under
/// `startup`, what it names `steady` is read apart from the frame loop's `frames` before it.
#[test]
fn profile_phase_splits_the_allocation_profile() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("ree-phase-test-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = root.join("Phase.luau");
    std::fs::write(
        &harness,
        "local Keep = {}\n\
         local function Setup() for I = 1, 2000 do Keep[1] = {I, I, I, I} end end\n\
         local function Frame() for I = 1, 3000 do Keep[1] = {I, I} end end\n\
         local function Steady() for I = 1, 4000 do Keep[1] = {I, I, I, I, I, I} end end\n\
         return function(Context)\n\
         local RunService = game:GetService('RunService')\n\
         Setup()\n\
         RunService.Heartbeat:Wait()\n\
         Frame()\n\
         Context.ProfilePhase('steady')\n\
         RunService.Heartbeat:Wait()\n\
         Steady()\n\
         end",
    )
    .unwrap();
    let profile = root.join("alloc");
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let emulator = crate::Emulator::start(engine, &crate::mount::ProjectSpec::default()).expect("emulator");
    crate::luau_profile::begin_allocations(profile.clone(), 1);
    let guard = crate::luau_profile::ProfileGuard;
    crate::luau_profile::hook_allocations(&emulator.lua);
    let args = super::run::RecordArgs {
        engine: crate::launch::Args::default(),
        harness: harness.to_string_lossy().into_owned(),
        out: None,
        seconds: 1.0,
        fps: 30.0,
        context: None,
        model_assets: Default::default(),
        animations: Vec::new(),
        frame_times: None,
        script_timeout: None,
        script_timeouts: None,
        timing_check: false,
        clock_probe: None,
    };
    let ran = super::run::run(&emulator.lua, &emulator.engine, &args, None).expect("run");
    drop(guard);
    assert_eq!(ran.status, "ok", "{:?}", ran.failure);
    let text = std::fs::read_to_string(&profile).expect("profile");
    let _ = std::fs::remove_dir_all(&root);
    // bytes the harness's functions allocated, by the phase at the head of their stacks
    let bytes = |phase: &str, function: &str| -> u64 {
        text.lines()
            .filter(|line| line.starts_with(&format!("{phase};")) && line.contains(&format!(";{function} ")))
            .filter_map(|line| line.rsplit_once(' ').and_then(|row| row.1.parse::<u64>().ok()))
            .sum()
    };
    assert!(bytes("startup", "Setup") > 100_000, "{text}");
    assert!(bytes("frames", "Frame") > 100_000, "{text}");
    assert!(bytes("steady", "Steady") > 200_000, "{text}");
    assert_eq!(bytes("steady", "Setup") + bytes("steady", "Frame") + bytes("frames", "Steady") + bytes("startup", "Steady"), 0, "{text}");
}

/// Context.Census files live threads, connections, handler runs, pending timers and instances under the line
/// that made them; what was let go (a finished thread, a disconnected handler, a Destroyed folder) is not
/// counted.
#[test]
fn census_names_the_line_behind_what_accumulates() {
    let root = std::env::temp_dir().join(format!("ree-census-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = root.join("Census.luau");
    std::fs::write(
        &harness,
        "local Kept = {}\n\
         return function(Context)\n\
         local Take = Context.Census(1)\n\
         local RunService = game:GetService('RunService')\n\
         for I = 1, 3 do\n\
         RunService.Heartbeat:Connect(function() end)\n\
         task.spawn(function() while true do task.wait(1) end end)\n\
         task.delay(100, function() end)\n\
         table.insert(Kept, coroutine.create(function() coroutine.yield() end))\n\
         local Folder = Instance.new('Folder') Folder.Name = 'Kept' Folder.Parent = workspace\n\
         task.spawn(function() end)\n\
         local Gone = RunService.Heartbeat:Connect(function() end)\n\
         local Dropped = Instance.new('Folder') Dropped.Parent = workspace\n\
         task.wait(0.5)\n\
         Gone:Disconnect() Dropped:Destroy()\n\
         end\n\
         print(Take())\n\
         end",
    )
    .unwrap();
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let emulator = crate::Emulator::start(engine, &crate::mount::ProjectSpec::default()).expect("emulator");
    let args = super::run::RecordArgs {
        engine: crate::launch::Args::default(),
        harness: harness.to_string_lossy().into_owned(),
        out: None,
        seconds: 2.0,
        fps: 60.0,
        context: None,
        model_assets: Default::default(),
        animations: Vec::new(),
        frame_times: None,
        script_timeout: None,
        script_timeouts: None,
        timing_check: false,
        clock_probe: None,
    };
    let ran = super::run::run(&emulator.lua, &emulator.engine, &args, None).expect("run");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(ran.status, "ok", "{:?}", ran.failure);
    let lines: Vec<String> = emulator.output().iter().map(|line| line.message.clone()).collect();
    let census: Json = serde_json::from_str(lines.last().expect("output")).expect("census json");
    let rows = |key: &str| census[key].as_array().cloned().unwrap_or_default();
    let line = |row: &Json, column: usize| row[column].as_str().unwrap_or("").rsplit(':').next().unwrap_or("").to_string();
    let count = |row: &Json| row.as_array().and_then(|r| r.last()).and_then(Json::as_u64).unwrap_or(0);
    // threads: [site, fn, at, status, held, count]
    let threads = rows("threads");
    let thread = |site: &str| threads.iter().find(|row| line(row, 0) == site).cloned();
    let looping = thread("7").expect("the loop's thread");
    assert_eq!((looping[2].as_str().map(|at| at.ends_with(":7")), looping[3].as_str(), looping[4].as_str(), count(&looping)), (Some(true), Some("suspended"), Some("timer"), 3), "{threads:?}");
    let delayed = thread("8").expect("the delayed thread");
    assert_eq!((delayed[4].as_str(), count(&delayed)), (Some("delay"), 3), "{threads:?}");
    let kept = thread("9").expect("the script-held coroutine");
    assert_eq!((kept[4].as_str(), count(&kept)), (Some(""), 3), "{threads:?}");
    assert!(thread("11").is_none(), "a finished thread is not counted: {threads:?}");
    // connections: [site, fn, signal, class, count]; the disconnected one is gone
    let connections = rows("connections");
    assert_eq!(connections.len(), 1, "{connections:?}");
    assert_eq!((line(&connections[0], 0), connections[0][2].as_str(), connections[0][3].as_str(), count(&connections[0])), ("6".to_string(), Some("Heartbeat"), Some("RunService"), 3));
    // runs since the call: each connection ran every Heartbeat it was connected for
    let runs = rows("runs");
    let ran_at = |site: &str| runs.iter().filter(|row| line(row, 0) == site).map(count).sum::<u64>();
    assert!(ran_at("6") > 100 && ran_at("12") > 50, "{runs:?}");
    // timers: [kind, at or fn, count, latest due]
    let timers = rows("timers");
    assert!(timers.iter().any(|row| row[0] == "delay" && line(row, 1) == "8" && row[2].as_u64() == Some(3)), "{timers:?}");
    // instances: [class, root, count]
    let instances = rows("instances");
    let folders: u64 = instances.iter().filter(|row| row[0] == "Folder" && row[1].as_str().is_some_and(|r| r.starts_with("Workspace"))).map(count).sum();
    assert_eq!(folders, 3, "{instances:?}");
}

/// Context.Holders names what keeps each orphan tree alive: a module table's field (the Luau way, printed from its
/// seed), another instance's property, and nothing for a tree no one holds (freed, so not listed); a tree whose
/// property holder let go is freed by the next report.
#[test]
fn holders_name_what_keeps_an_orphan_tree_alive() {
    let root = std::env::temp_dir().join(format!("ree-holders-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = root.join("Holders.luau");
    std::fs::write(
        &harness,
        "local Table = {}\n\
         return function(Context)\n\
         Table.Part = Instance.new('Part') Table.Part.Name = 'Planted'\n\
         local Model = Instance.new('Model') Model.Name = 'Owner' Table.Model = Model\n\
         local Primary = Instance.new('Part') Primary.Name = 'Planted'\n\
         Model.PrimaryPart = Primary\n\
         local Value = Instance.new('ObjectValue') Value.Parent = workspace Value.Value = Instance.new('Part')\n\
         Value.Value.Name = 'Planted'\n\
         for I = 1, 5 do Instance.new('Part').Name = 'Planted' end\n\
         local Report = game:GetService('HttpService'):JSONDecode(Context.Holders({'Part Planted'}, 2))\n\
         print(game:GetService('HttpService'):JSONEncode(Report))\n\
         Value.Value = nil\n\
         local Again = game:GetService('HttpService'):JSONDecode(Context.Holders({'Part Planted'}, 2, true))\n\
         print(game:GetService('HttpService'):JSONEncode(Again))\n\
         end",
    )
    .unwrap();
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let emulator = crate::Emulator::start(engine, &crate::mount::ProjectSpec::default()).expect("emulator");
    let args = super::run::RecordArgs {
        engine: crate::launch::Args::default(),
        harness: harness.to_string_lossy().into_owned(),
        out: None,
        seconds: 1.0,
        fps: 60.0,
        context: None,
        model_assets: Default::default(),
        animations: Vec::new(),
        frame_times: None,
        script_timeout: None,
        script_timeouts: None,
        timing_check: false,
        clock_probe: None,
    };
    let ran = super::run::run(&emulator.lua, &emulator.engine, &args, None).expect("run");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(ran.status, "ok", "{:?}", ran.failure);
    let lines: Vec<String> = emulator.output().iter().map(|line| line.message.clone()).collect();
    let reports: Vec<Json> = lines.iter().filter_map(|line| serde_json::from_str(line).ok()).filter(|report: &Json| report["type"] == "holders").collect();
    assert_eq!(reports.len(), 2, "{lines:?}");
    let group = &reports[0]["groups"][0];
    // Table.Part (the module's table), Owner's PrimaryPart and the ObjectValue's part; the five dropped parts are gone
    assert_eq!(group["roots"], 3, "{group}");
    assert_eq!(group["explained"]["lua"], 1, "{group}");
    assert_eq!(group["explained"]["property"], 2, "{group}");
    assert!(group["ways"][0]["way"].as_str().is_some_and(|way| way.contains("upvalue") && way.contains("[\"Part\"]")), "{group}");
    let holders: Vec<&str> = group["otherHolders"].as_array().unwrap().iter().filter_map(|holder| holder["example"].as_str()).collect();
    assert!(holders.iter().any(|holder| holder.contains("PrimaryPart")), "{holders:?}");
    assert!(holders.iter().any(|holder| holder.contains("ObjectValue") && holder.contains("Value")), "{holders:?}");
    // The ObjectValue let go: that part is freed before the second report lists anything, and the others stay
    let again = &reports[1]["groups"][0];
    assert_eq!(again["roots"], 2, "{again}");
    assert_eq!(again["explained"]["unexplained"], serde_json::Value::Null, "{again}");
}

/// Context.SetRandomSeed(Seed) restarts math.random and the unseeded Random.new() sequence from that seed, so one
/// service varies its runs by seed; a server mount gives scripts and folders the Studio names of the project's
/// name map ([`crate::project::STUDIO_NAME_MAP`]: names a file system cannot hold), and a client mount keeps the file names.
#[test]
fn a_harness_reseeds_its_run_and_a_server_mount_takes_studio_names() {
    let root = std::env::temp_dir().join(format!("ree-seed-names-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let write = |relative: &str, text: &str| {
        let file = root.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write("place/ServerScriptService/Handlers/A.B.C/init.luau", "return 'handler'");
    write("place/ServerScriptService/Handlers/A.B.C/Events.luau", "return 'events'");
    write("place/ServerScriptService/Handlers/Gaze Stone.luau", "return 'gaze'");
    let entry = |disk: &str, studio: &[&str]| format!("\"{disk}\": {{ \"studioPathParts\": [{}] }}", studio.iter().map(|part| format!("\"{part}\"")).collect::<Vec<_>>().join(", "));
    write(
        crate::project::STUDIO_NAME_MAP,
        &format!(
            "{{ \"version\": 1, \"files\": {{ {}, {}, {}, {} }} }}",
            entry("place/ServerScriptService/Handlers/A.B.C/init.luau", &["place", "ServerScriptService", "Handlers", "A.B.C.", "init.luau"]),
            entry("place/ServerScriptService/Handlers/A.B.C/Events.luau", &["place", "ServerScriptService", "Handlers", "A.B.C.", "Events.luau"]),
            entry("place/ServerScriptService/Handlers/Gaze Stone.luau", &["place", "ServerScriptService", "Handlers", "Gaze: Stone.luau"]),
            entry("other/ServerScriptService/Handlers/Elsewhere.luau", &["other", "ServerScriptService", "Handlers", "Else: where.luau"]),
        ),
    );
    write(
        "Harness.luau",
        "return function(Context)\n\
         local function Draw() return `{math.random(1, 1000000)} {Random.new():NextInteger(1, 1000000)} {Random.new():NextInteger(1, 1000000)}` end\n\
         Context.SetRandomSeed(7)\n\
         local First = Draw()\n\
         Context.SetRandomSeed(7)\n\
         local Again = Draw()\n\
         Context.SetRandomSeed(8)\n\
         local Other = Draw()\n\
         print('seeds', First == Again, First ~= Other, pcall(Context.SetRandomSeed, 0 / 0))\n\
         local Names = {}\n\
         for _, Child in game:GetService('ServerScriptService').Handlers:GetChildren() do table.insert(Names, Child.Name) end\n\
         table.sort(Names)\n\
         print('names', table.concat(Names, ','))\n\
         end",
    );
    let place = root.join("place").to_string_lossy().into_owned();
    let harness = root.join("Harness.luau").to_string_lossy().into_owned();
    let mut printed = Vec::new();
    for run_context in ["server", "client"] {
        let args: Vec<String> = ["--harness", &harness, "--roots", &place, "--seconds", "0.05", "--run-context", run_context].iter().map(|s| s.to_string()).collect();
        let parsed = super::run::parse(&args, false).expect("arguments");
        let (done, emulator) = super::run::execute_with(&parsed, &crate::launch::Host::default(), false, false).expect("run");
        let mut bytes = Vec::new();
        done.print(&mut bytes).expect("printed");
        drop(emulator);
        let summary: Json = serde_json::from_str(&String::from_utf8(bytes).expect("UTF-8")).expect("one line of JSON");
        for line in summary["output"].as_array().expect("output") {
            printed.push(format!("{run_context} {}: {}", line["kind"].as_str().unwrap_or_default(), line["message"].as_str().unwrap_or_default()));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(printed.iter().any(|line| line.starts_with("server print: seeds true true false ") && line.contains("Context.SetRandomSeed: a finite number expected")), "{printed:?}");
    for wanted in [
        "server print: names A.B.C.,Gaze: Stone",
        "client print: names A.B.C,Gaze Stone",
    ] {
        assert!(printed.iter().any(|line| line == wanted), "{wanted}: {printed:?}");
    }
    assert!(!printed.iter().any(|line| line.starts_with("server error:") || line.starts_with("client error:")), "{printed:?}");
}

/// Context.FireEvent(Instance, Event, ...) fires an engine event nothing here raises (a walking Humanoid's Running) to the
/// connections made to it, with its arguments; an unknown event or a table argument raises.
#[test]
fn a_harness_fires_an_engine_event_to_its_connections() {
    let root = std::env::temp_dir().join(format!("ree-fire-event-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join("place")).unwrap();
    std::fs::write(
        root.join("Harness.luau"),
        "return function(Context)\n\
         local Model = Instance.new('Model')\n\
         local Humanoid = Instance.new('Humanoid')\n\
         Humanoid.Parent = Model\n\
         Model.Parent = workspace\n\
         local Speeds = {}\n\
         Humanoid.Running:Connect(function(Speed) table.insert(Speeds, Speed) end)\n\
         Context.FireEvent(Humanoid, 'Running', 22.5)\n\
         Context.FireEvent(Humanoid, 'Running', 0)\n\
         task.wait()\n\
         local _, Unknown = pcall(Context.FireEvent, Humanoid, 'NoSuchEventAnywhere')\n\
         local _, Table = pcall(Context.FireEvent, Humanoid, 'Running', {})\n\
         print('speeds', table.concat(Speeds, ','), Unknown, Table)\n\
         end",
    )
    .unwrap();
    let place = root.join("place").to_string_lossy().into_owned();
    let harness = root.join("Harness.luau").to_string_lossy().into_owned();
    let args: Vec<String> = ["--harness", &harness, "--roots", &place, "--seconds", "0.1"].iter().map(|s| s.to_string()).collect();
    let parsed = super::run::parse(&args, false).expect("arguments");
    let (done, emulator) = super::run::execute_with(&parsed, &crate::launch::Host::default(), false, false).expect("run");
    let mut bytes = Vec::new();
    done.print(&mut bytes).expect("printed");
    drop(emulator);
    let _ = std::fs::remove_dir_all(&root);
    let summary: Json = serde_json::from_str(&String::from_utf8(bytes).expect("UTF-8")).expect("one line of JSON");
    let printed: Vec<String> = summary["output"].as_array().expect("output").iter().map(|line| line["message"].as_str().unwrap_or_default().to_string()).collect();
    let line = printed.iter().find(|line| line.starts_with("speeds ")).unwrap_or_else(|| panic!("{printed:?}"));
    assert!(line.starts_with("speeds 22.5,0 "), "{line}");
    assert!(line.contains("no event named NoSuchEventAnywhere") && line.contains("arguments are nil, booleans"), "{line}");
}

/// A Studio-only placeholder (the place's hierarchy snapshot gives it a class and a name and no values) is built when a script
/// first lists its parent, so a run that looks at more of the place built more of them, and recorded them appearing then: a cast
/// that walked every service had its recording carry the place's seven Lighting effects (drawn at their default values) and two of
/// the Workspace's own, 8 actions and 10 scene instances that another version of the same code, which walked nothing, did not. They
/// are the place's own now: one that draws is never drawn, and a folder or model only once something the run made is under it
/// (an effect parented into the place's Visuals folder keeps its path), so the recording is the same whether they were built
/// before the run or during it.
#[test]
fn studio_only_placeholders_are_not_drawn_whenever_they_are_built() {
    let source = r#"
        local Entities = Instance.new("Folder")
        Entities.Name = "Entities"
        Entities.Parent = workspace
        task.delay(0.1, function()
            local Circle = Instance.new("Part")
            Circle.Name = "Circle"
            Circle.Anchored = true
            Circle.Parent = Entities:WaitForChild("Circles")
            Circle.Position = Vector3.new(4, 0, 0)
        end)
    "#;
    let run = |built_at: u32| -> (Json, Vec<u8>) {
        let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
        let lua = Lua::new();
        engine_lua::start(&lua, &engine).expect("start");
        let recorder = Recorder::install(&engine, 60.0);
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            crate::instance::create_named(&mut state, &engine.reflection, class, "Harness")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let function = lua.load(source).set_name("=Harness").set_environment(env).into_function().expect("compile");
        let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        assert!(!matches!(call.outcome().unwrap(), CallOutcome::Failed(..)), "the harness runs");
        // the place's own: a folder and a highlight in the Workspace's Entities, a blur in Lighting (what project::materialize builds)
        let build = |engine: &Engine| {
            let lighting = crate::services::find_service(engine, engine.world().game, "Lighting").expect("Lighting");
            let entities = engine.state.borrow().dom.find_child(engine.world().workspace, b"Entities").expect("Entities");
            let mut state = engine.state.borrow_mut();
            for (class, name, parent) in
                [("Folder", "Circles", entities), ("Folder", "Empty", entities), ("Highlight", "Highlight", entities), ("BlurEffect", "Blur", lighting)]
            {
                let class = engine.reflection.class_id(class).unwrap();
                let id = crate::instance::create_named(&mut state, &engine.reflection, class, name);
                state.dom.at_mut(id).set_flag(crate::dom::flags::STUDIO_ONLY, true);
                // (as project::materialize attaches a placeholder: silently)
                state.dom.attach(id, parent);
            }
        };
        if built_at == 0 {
            build(&engine);
        }
        recorder.sample(&engine, 0);
        for frame in 1..=12 {
            if frame == built_at {
                build(&engine);
            }
            scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("frame");
            recorder.sample(&engine, frame);
        }
        let unique = TEST_DIRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let out = std::env::temp_dir().join(format!("rur-record-test-{}-{}-{unique}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        recorder.write(&engine, &out, Map::new()).expect("write");
        let playback: Json = serde_json::from_str(&std::fs::read_to_string(out.join("playback.json")).unwrap()).unwrap();
        let scene = std::fs::read(out.join("scene.rbxm")).unwrap();
        let _ = std::fs::remove_dir_all(&out);
        drop(lua);
        engine.teardown();
        (playback, scene)
    };
    let (before, before_scene) = run(0);
    let (during, during_scene) = run(4);
    assert_eq!(before, during, "the same playback whenever the placeholders were built");
    assert_eq!(before_scene, during_scene, "the same scene whenever the placeholders were built");
    let scene = rbx_binary::from_reader(during_scene.as_slice()).expect("scene.rbxm");
    let names: Vec<String> = scene.descendants().map(|instance| instance.name.clone()).collect();
    for placeholder in ["Empty", "Highlight", "Blur"] {
        assert!(!names.iter().any(|name| name == placeholder), "{placeholder} is not drawn: {names:?}");
    }
    instance(&scene, &["Entities", "Circles", "Circle"]);
    assert!(find(&during["actions"], |a| a["target"].as_str().is_some_and(|t| t.contains("Blur") || t.contains("Highlight"))).is_empty(), "no action for a placeholder");
}

#[test]
fn a_motion_key_is_kept_by_distance_however_far_the_part_is_from_the_origin() {
    let key = |at: f64, x: f64, y: f64, z: f64| {
        let mut cframe = vec![x, y, z];
        cframe.extend([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        serde_json::json!({ "at": at, "cframe": cframe })
    };
    let far = 400.0;
    let curve: Vec<Json> = (0..120)
        .map(|frame| {
            let at = f64::from(frame) / 60.0;
            key(at, far, far + 1.5 * at * at, -far)
        })
        .collect();
    let kept = super::reduce(curve.clone(), "cframe", super::motion_allowance);
    let times: Vec<f64> = kept.iter().map(|kept| kept["at"].as_f64().unwrap()).collect();
    for original in &curve {
        let at = original["at"].as_f64().unwrap();
        let index = times.partition_point(|time| *time <= at).clamp(1, times.len() - 1);
        let alpha = (at - times[index - 1]) / (times[index] - times[index - 1]);
        let (from, to) = (kept[index - 1]["cframe"][1].as_f64().unwrap(), kept[index]["cframe"][1].as_f64().unwrap());
        let strayed = (from + (to - from) * alpha - original["cframe"][1].as_f64().unwrap()).abs();
        assert!(strayed <= super::MOTION_POSITION_STUDS + 1e-9, "at {at} the line strays {strayed} studs");
    }
    let straight: Vec<Json> = (0..120)
        .map(|frame| {
            let at = f64::from(frame) / 60.0;
            key(at, far + at, far, -far)
        })
        .collect();
    assert_eq!(super::reduce(straight, "cframe", super::motion_allowance).len(), 2, "a straight line needs only its ends");
}
