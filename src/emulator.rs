//! An engine, its Luau VM and a mounted project in one value: what a program embedding the
//! emulator holds. Dropping it drops the VM first, then the engine's own cycles
//! ([`Engine::teardown`]), so a process can run many in turn.
//!
//! ```no_run
//! use roblox_engine_emulator::{Emulator, Engine, Options, mount::ProjectSpec};
//! let engine = Engine::new(Options { mode: "play".into(), ..Options::default() })?;
//! let spec = ProjectSpec { roots: vec!["C:/game/shared".into(), "C:/game/src".into()], ..Default::default() };
//! let emulator = Emulator::start(engine, &spec)?;
//! let call = emulator.run("Probe", "print(workspace.Gravity) task.wait(1) print('a second later')")?;
//! emulator.run_until(2.0)?;
//! assert!(call.is_finished()?);
//! for line in emulator.output() {
//!     println!("{} {}", line.kind, line.message);
//! }
//! # Ok::<(), String>(())
//! ```

use std::{mem::ManuallyDrop, rc::Rc, time::Instant};

use mlua::prelude::*;

use crate::{
    Engine, K, NameId, instance as core,
    lua::{
        globals::{Call, CallOutcome, call},
        modules,
    },
    mount::{self, Project, ProjectSpec},
    output::Entry,
    scheduler,
    value::{Str, Value},
};

pub struct Emulator {
    pub engine: Rc<Engine>,
    pub project: Project,
    pub lua: ManuallyDrop<Lua>,
}

impl Drop for Emulator {
    fn drop(&mut self) {
        // the script timeout's watchdog stops writing the VM's interrupt before the VM goes
        if let Some(timeout) = self.engine.state.try_borrow().ok().and_then(|state| state.sched.timeout.clone()) {
            timeout.detach();
        }
        // a loopback run's callers waiting on the other side, held for this VM, go before it
        crate::loopback::release(&self.engine);
        // SAFETY: the field is never used again (this is the Emulator's drop)
        unsafe { ManuallyDrop::drop(&mut self.lua) };
        self.engine.teardown();
    }
}

fn lua_error(error: LuaError) -> String {
    scheduler::error_message(&error)
}

impl Emulator {
    /// Engine.Start and the project mount of `spec` in `engine` (made by [`Engine::new`] or
    /// [`Engine::with_layers`]).
    pub fn start(engine: Rc<Engine>, spec: &ProjectSpec) -> Result<Emulator, String> {
        let lua = Lua::new();
        match mount::mount_project(&lua, &engine, spec, Instant::now()) {
            Ok(project) => Ok(Emulator { engine, project, lua: ManuallyDrop::new(lua) }),
            Err(error) => {
                drop(lua);
                engine.teardown();
                Err(error)
            }
        }
    }

    /// Runs `source` as the code of a ModuleScript named `name` (a script environment: `script`,
    /// `require` and the engine's globals) on a scheduler thread, as Roblox runs a script: it may
    /// yield, and the deferred queue is flushed after it. The [`Call`] tells how it ended so far.
    pub fn run(&self, name: &str, source: &str) -> Result<Call, String> {
        let (lua, engine) = (&*self.lua, &self.engine);
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").ok_or("no ModuleScript class")?;
            let script = core::create(&mut state, &engine.reflection, class);
            core::raw_set_silent(&mut state, &engine.reflection, script, K::Name as NameId, Value::String(Str::from(name)));
            state.dom.at_mut(script).set_source(Some(Rc::from(source.as_bytes())));
            script
        };
        let env = modules::environment(lua, engine, script).map_err(lua_error)?;
        let function = lua.load(source).set_name(format!("={name}")).set_environment(env).into_function().map_err(lua_error)?;
        call(lua, engine, function, LuaMultiValue::new()).map_err(lua_error)
    }

    /// One frame of `dt` seconds (Heartbeat, the render step and the deferred queue, as the mode runs
    /// them).
    pub fn step(&self, dt: f64) -> Result<(), String> {
        scheduler::step(&self.lua, &self.engine, Some(dt)).map_err(|error| lua_error(error))
    }

    /// Runs virtual time to `time` seconds at the default frame rate.
    pub fn run_until(&self, time: f64) -> Result<(), String> {
        scheduler::run_until(&self.lua, &self.engine, time).map_err(|error| lua_error(error))
    }

    /// The virtual clock (seconds since the start).
    pub fn now(&self) -> f64 {
        self.engine.state.borrow().sched.now
    }

    /// Everything scripts printed, warned and raised so far.
    pub fn output(&self) -> Vec<Entry> {
        self.engine.state.borrow().output.entries.clone()
    }
}

impl Call {
    /// Whether the call returned (not failed, not still yielding).
    pub fn is_finished(&self) -> Result<bool, String> {
        Ok(matches!(self.outcome().map_err(|error| lua_error(error))?, CallOutcome::Finished(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    #[test]
    fn a_script_runs_yields_and_reports() {
        let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).unwrap();
        let emulator = Emulator::start(engine, &ProjectSpec::default()).unwrap();
        let call = emulator.run("Probe", "print(game:GetService('RunService'):IsRunning()) local Elapsed = task.wait(0.5) warn('later', Elapsed > 0.49)").unwrap();
        assert!(!call.is_finished().unwrap(), "yielding in task.wait");
        emulator.run_until(1.0).unwrap();
        assert!(call.is_finished().unwrap());
        let lines: Vec<String> = emulator.output().iter().map(|line| format!("{}: {}", line.kind, line.message)).collect();
        assert_eq!(lines, ["print: true", "warn: later true"]);
        assert!(emulator.now() >= 1.0);
    }

    /// A client's and a server's Emulator with an empty project, a script run in each: their output
    fn start_and_run(server: bool, source: &str) -> Vec<String> {
        let options = Options {
            mode: "play".into(),
            run_context: if server { crate::RunContext::Server } else { crate::RunContext::Client },
            local_player_name: if server { None } else { Some("Player".into()) },
            ..Options::default()
        };
        let emulator = Emulator::start(Engine::new(options).unwrap(), &ProjectSpec::default()).unwrap();
        emulator.run("Probe", source).unwrap();
        emulator.run_until(0.1).unwrap();
        emulator.output().iter().map(|line| format!("{}: {}", line.kind, line.message)).collect()
    }

    /// The engine's own Luau (prelude, services, server, vector3) compiles once a process (lua::modules::load_engine_luau):
    /// a second client and a second server compile none of it, and their scripts see what the first ones saw.
    #[test]
    fn a_second_engine_compiles_no_engine_luau_and_runs_the_same() {
        let probe = "local V = Vector3.new(1, 2, 3) print(typeof(V), V.Magnitude, V:Dot(Vector3.one), CFrame.new(V):Inverse().Position) \
                     print(game:GetService('RunService'):IsServer(), #game:GetService('Players'):GetPlayers()) \
                     task.wait(0.05) print(pcall(function() return game:GetService('DataStoreService'):GetDataStore('A'):GetAsync('k') end))";
        let first = (start_and_run(false, probe), start_and_run(true, probe));
        // (counted on this thread: other tests start engines meanwhile)
        let compiled = crate::lua::modules::engine_compiles_here();
        let second = (start_and_run(false, probe), start_and_run(true, probe));
        assert_eq!(crate::lua::modules::engine_compiles_here(), compiled, "a second engine compiled engine Luau again");
        assert!(crate::lua::modules::engine_compiles() >= 4, "the four chunks compiled");
        assert_eq!(first, second);
        assert!(first.0.iter().any(|line| line == "print: false 1") && first.1.iter().any(|line| line == "print: true 0"), "{first:?}");
    }

    /// What starting an engine with an empty project costs, client and server (Engine::new and Emulator::start, the engine's
    /// own Luau compiled to native code as `ree run` has it): `cargo test --release engine_start_times -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn engine_start_times() {
        for server in [false, true] {
            let mut times: Vec<f64> = (0..9)
                .map(|_| {
                    let options = Options {
                        mode: "play".into(),
                        run_context: if server { crate::RunContext::Server } else { crate::RunContext::Client },
                        local_player_name: if server { None } else { Some("Player".into()) },
                        ..Options::default()
                    };
                    let started = Instant::now();
                    let emulator = Emulator::start(Engine::new(options).unwrap(), &ProjectSpec::default()).unwrap();
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    drop(emulator);
                    elapsed
                })
                .collect();
            // (the first start reads the reflection dump)
            times.remove(0);
            times.sort_by(f64::total_cmp);
            eprintln!("{} start: min {:.2} ms, median {:.2} ms", if server { "server" } else { "client" }, times[0], times[times.len() / 2]);
        }
    }

    /// A project mounts on a thread pool whose only thread is the mounting one. The mount lists its
    /// roots on the pool during Engine.Start (project::Prescan), and rur's browser module builds
    /// rayon's pool on its page's thread (`use_current_thread`), which runs a job it spawned only when
    /// it yields to rayon: the page waited for the first root's listing forever.
    #[test]
    fn a_project_mounts_on_a_pool_of_the_mounting_thread_alone() {
        let root = std::env::temp_dir().join(format!("ree-prescan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ReplicatedStorage/Shared")).unwrap();
        std::fs::write(root.join("ReplicatedStorage/Shared/Value.luau"), "return 4").unwrap();
        let spec = ProjectSpec { roots: vec![root.to_string_lossy().into_owned()], ..Default::default() };
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _pool = rayon::ThreadPoolBuilder::new().num_threads(1).use_current_thread().build().unwrap();
            let emulator = Emulator::start(Engine::new(Options::default()).unwrap(), &spec).unwrap();
            let call = emulator.run("Probe", "print(require(game:GetService('ReplicatedStorage').Shared.Value))").unwrap();
            let finished = call.is_finished().unwrap();
            drop(send.send((finished, emulator.output().iter().map(|line| format!("{}: {}", line.kind, line.message)).collect::<Vec<_>>())));
        });
        let (finished, lines) = match receive.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("the mount waited 30 s for a listing no thread ran"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("the mounting thread failed"),
        };
        let _ = std::fs::remove_dir_all(&root);
        assert!(finished && lines.iter().any(|line| line == "print: 4"), "{lines:?}");
    }
}
