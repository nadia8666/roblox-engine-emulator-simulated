//! The counters the CPU's built-in list lacks (counters.rs: the stall cycles, the miss buffer's occupancy and
//! allocations, the demand fills by source, the load-store dispatch: events of AMD Family 1Ah) are registered with
//! Windows the way its own tools do it: Windows Performance Recorder (wpr.exe, part of Windows) reads a profile's
//! micro-architectural counter definitions and adds them to the CPU's profile sources until the next restart. The service
//! writes a profile of the ones the CPU does not offer yet beside itself and runs `wpr -profiledetails` on it, which
//! starts no trace (those a service registered before stay: they are left out, so none is registered twice).
//! Registering needs the system's profiling right, which the service has and a console does not.

use std::fmt::Write as _;

use crate::{etw, ffi, protocol};

/// The CPU's vendor and family (CPUID leaves 0 and 1)
pub fn cpu() -> (String, u32) {
    #[cfg(target_arch = "x86_64")]
    {
        #[allow(unused_unsafe)]
        let (vendor, signature) = unsafe { (std::arch::x86_64::__cpuid(0), std::arch::x86_64::__cpuid(1)) };
        let mut name = Vec::with_capacity(12);
        for register in [vendor.ebx, vendor.edx, vendor.ecx] {
            name.extend_from_slice(&register.to_le_bytes());
        }
        let base = (signature.eax >> 8) & 0xF;
        let family = base + if base == 0xF { (signature.eax >> 20) & 0xFF } else { 0 };
        (String::from_utf8_lossy(&name).into_owned(), family)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        (String::new(), 0)
    }
}

/// The CPUs the custom counters are defined for: AMD Family 1Ah (Zen 5)
pub fn custom_counters_fit() -> bool {
    let (vendor, family) = cpu();
    vendor == "AuthenticAMD" && family == 0x1A
}

/// The registration profile of every custom counter (`ree-hw-counters profile`)
pub fn profile() -> String {
    profile_of(&protocol::KNOWN.iter().filter(|known| known.custom.is_some()).collect::<Vec<_>>())
}

/// The registration profile of `counters` (custom ones), as micro-architectural counters WPR adds (schema: WPR's own)
pub fn profile_of(counters_wanted: &[&protocol::Known]) -> String {
    let mut sources = String::new();
    let mut counters = String::new();
    for known in counters_wanted.iter().filter(|known| known.custom.is_some()) {
        let custom = known.custom.unwrap();
        let _ = writeln!(sources, "        <ProfileSource Name=\"{}\" Event=\"0x{:X}\" Unit=\"0x{:02X}\" Interval=\"0x02000003\"/>", known.source, custom.event, custom.unit);
        let _ = writeln!(counters, "        <Counter Value=\"{}\"/>", known.source);
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<WindowsPerformanceRecorder Version="1.0" Author="ree-hw-counters" Comments="Registers the AMD counters ree's hardware-counter profiles read">
  <Profiles>
    <SystemCollector Id="ReeSystemCollector" Name="ree counters registration">
      <BufferSize Value="64"/>
      <Buffers Value="16"/>
    </SystemCollector>
    <SystemProvider Id="ReeSystemProvider">
      <Keywords>
        <Keyword Value="CSwitch"/>
      </Keywords>
    </SystemProvider>
    <MicroArchitecturalConfig Id="ReeCounters" Strict="true">
      <ProfileSources Architecture="AMD" Description="AMD Family 1Ah: retire stalls, the L1 data cache's miss buffer, demand fills by source, load-store dispatch">
{sources}      </ProfileSources>
    </MicroArchitecturalConfig>
    <HardwareCounter Id="ReeHardwareCounter" Strict="true">
      <MicroArchitecturalConfigId Value="ReeCounters"/>
      <Counters>
{counters}      </Counters>
      <Events>
        <Event Value="CSwitch"/>
      </Events>
    </HardwareCounter>
    <Profile Id="ReeCounters.Verbose.Memory" Name="ReeCounters" Description="Registers the counters ree reads" LoggingMode="Memory" DetailLevel="Verbose">
      <Collectors>
        <SystemCollectorId Value="ReeSystemCollector">
          <SystemProviderId Value="ReeSystemProvider"/>
          <HardwareCounterId Value="ReeHardwareCounter"/>
        </SystemCollectorId>
      </Collectors>
    </Profile>
  </Profiles>
</WindowsPerformanceRecorder>
"#
    )
}

/// This executable's folder
fn own_folder() -> Result<std::path::PathBuf, String> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe { ffi::GetModuleFileNameW(std::ptr::null_mut(), buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("the service's own path".into());
    }
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
    path.parent().map(|folder| folder.to_path_buf()).ok_or_else(|| "the service's folder".into())
}

/// Runs `wpr.exe` (from the system folder, never a path anyone else chose) with `arguments`: its exit code
fn run_wpr(arguments: &str) -> Result<u32, String> {
    let mut system = vec![0u16; 1024];
    let length = unsafe { ffi::GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) } as usize;
    if length == 0 || length >= system.len() {
        return Err("the system folder".into());
    }
    let wpr = format!("{}\\wpr.exe", String::from_utf16_lossy(&system[..length]));
    let application = ffi::wide(&wpr);
    let mut command = ffi::wide(&format!("\"{wpr}\" {arguments}"));
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    unsafe {
        let mut startup: ffi::StartupInfo = std::mem::zeroed();
        startup.cb = std::mem::size_of::<ffi::StartupInfo>() as u32;
        let mut information: ffi::ProcessInformation = std::mem::zeroed();
        if ffi::CreateProcessW(application.as_ptr(), command.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), 0, CREATE_NO_WINDOW, std::ptr::null_mut(), std::ptr::null(), &mut startup, &mut information) == 0 {
            return Err(format!("starting {wpr}: error {}", ffi::GetLastError()));
        }
        let (process, _thread) = (ffi::Owned(information.process), ffi::Owned(information.thread));
        if ffi::WaitForSingleObject(process.0, 60_000) != ffi::WAIT_OBJECT_0 {
            ffi::TerminateProcess(process.0, 1);
            return Err("wpr.exe did not finish in 60 s".into());
        }
        let mut code = 0u32;
        ffi::GetExitCodeProcess(process.0, &mut code);
        Ok(code)
    }
}

/// Makes sure the CPU offers every one of `wanted` (profile source names), registering the custom ones it lacks
pub fn ensure(wanted: &[String], notes: &mut Vec<String>) -> Result<(), String> {
    let offered = |list: &[(u32, String, u32, u32)], name: &str| list.iter().any(|(_, source, _, _)| source == name);
    let list = etw::sources()?;
    let missing: Vec<&String> = wanted.iter().filter(|name| !offered(&list, name)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let names = |list: &[(u32, String, u32, u32)]| list.iter().map(|(_, name, _, _)| name.as_str()).filter(|name| *name != "Timer").collect::<Vec<_>>().join(", ");
    if let Some(unknown) = missing.iter().find(|name| !protocol::KNOWN.iter().any(|known| known.source == name.as_str() && known.custom.is_some())) {
        return Err(format!("the CPU offers no counter {unknown} (it offers {})", names(&list)));
    }
    let (vendor, family) = cpu();
    if !custom_counters_fit() {
        let asked: Vec<&str> = missing.iter().map(|name| protocol::name_of(name)).collect();
        return Err(format!(
            "{} are defined for AMD Family 1Ah processors; this CPU is {vendor} family {family:#x}: ask for the counters it offers with --profile-counters ({})",
            asked.join(", "),
            names(&list)
        ));
    }
    // (only those missing: a source registered already is not registered again)
    let unregistered: Vec<&protocol::Known> = protocol::KNOWN.iter().filter(|known| known.custom.is_some() && !offered(&list, known.source)).collect();
    let path = own_folder()?.join("counters.wprp");
    std::fs::write(&path, profile_of(&unregistered)).map_err(|error| format!("writing {}: {error}", path.display()))?;
    let code = run_wpr(&format!("-profiledetails \"{}!ReeCounters\"", path.display()))?;
    let list = etw::sources()?;
    let still: Vec<&str> = wanted.iter().filter(|name| !offered(&list, name)).map(|name| name.as_str()).collect();
    if !still.is_empty() {
        let hint = if code == 0xC558_063A { ": adding them needs the system's profiling right, which the installed service has and a console does not" } else { "" };
        return Err(format!("registering {} with Windows Performance Recorder failed (wpr exit code {code:#010x}){hint}", still.join(", ")));
    }
    notes.push(format!("registered {} with Windows until the next restart", unregistered.iter().map(|known| known.name).collect::<Vec<_>>().join(", ")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile names every custom counter with its event and unit mask, the way WPR's schema takes them, and a
    /// profile of some names those only
    #[test]
    fn the_profile_defines_every_custom_counter() {
        let text = profile();
        assert!(text.contains(r#"<ProfileSource Name="ReeL2Misses" Event="0x43" Unit="0xFE" Interval="0x02000003"/>"#), "{text}");
        assert!(text.contains(r#"<ProfileSource Name="ReeL1dAccesses" Event="0x29" Unit="0x07""#));
        assert!(text.contains(r#"<ProfileSource Name="ReeLoadStallCycles" Event="0xD6" Unit="0xA2""#), "{text}");
        assert!(text.contains(r#"<ProfileSource Name="ReeMabOccupancy" Event="0x5F" Unit="0x00""#), "{text}");
        assert!(text.contains(r#"<ProfileSource Name="ReeMabAllocs" Event="0x41" Unit="0x0F""#), "{text}");
        assert!(text.contains(r#"<Counter Value="ReeLlcMisses"/>"#));
        assert!(text.contains(r#"Architecture="AMD""#));
        let stall = protocol::known("load-stall-cycles").unwrap();
        let some = profile_of(&[stall]);
        assert!(some.contains("ReeLoadStallCycles") && !some.contains("ReeL2Misses"), "{some}");
    }

    #[test]
    fn the_cpu_is_named() {
        let (vendor, _) = cpu();
        if cfg!(target_arch = "x86_64") {
            assert_eq!(vendor.len(), 12, "{vendor:?}");
        }
    }
}
