//! Running in the background: a Windows service started at boot
//! (docs/CAPTURE-SERVER.md §5.3). The setup page installs, starts, stops and
//! removes it; the same actions are `misterkvms-capture service …`.
//!
//! The service runs `misterkvms-capture serve --service --config <dir>` as
//! LocalSystem, so it shares the config folder with a copy started by
//! double-clicking. Changing the service needs an administrator: from a
//! normal copy the page asks Windows for it (a UAC prompt on
//! `misterkvms-capture service <action>`); from the service itself (already
//! LocalSystem) the action runs in a helper process, since stopping the
//! service ends the process that would report back.

use std::path::{Path, PathBuf};

use serde::Serialize;

pub const NAME: &str = "MiSTerKVMSCapture";
pub const DISPLAY_NAME: &str = "MiSTerKVMS Capture";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Action {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
}

impl Action {
    pub fn arg(self) -> &'static str {
        match self {
            Action::Install => "install",
            Action::Uninstall => "uninstall",
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Status => "status",
        }
    }

    pub fn parse(s: &str) -> Option<Action> {
        [Action::Install, Action::Uninstall, Action::Start, Action::Stop, Action::Restart, Action::Status].into_iter().find(|a| a.arg() == s)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// This OS has a service we manage (Windows only, for now).
    pub supported: bool,
    pub installed: bool,
    /// "running", "stopped", "starting", "stopping", or "" when not installed.
    pub state: String,
    /// This process is the service.
    pub this_is_service: bool,
    /// The program the service runs.
    pub exe: String,
}

#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod other {
    use super::*;

    pub fn running_as_service() -> bool {
        false
    }
    pub fn status() -> Status {
        Status { supported: false, installed: false, state: String::new(), this_is_service: false, exe: String::new() }
    }
    pub fn run(_dir: PathBuf) -> Result<(), String> {
        Err("running as a service is only supported on Windows for now".into())
    }
    pub fn cli(_dir: &Path, _action: Action) -> Result<(), String> {
        Err("running as a service is only supported on Windows for now".into())
    }
    pub fn request(_dir: &Path, _action: Action) -> Result<(), String> {
        Err("running as a service is only supported on Windows for now".into())
    }
    pub fn started_from_explorer() -> bool {
        false
    }
    pub fn no_throttling() {}
    pub fn open_browser(_url: &str) {}
}

#[cfg(not(windows))]
pub use other::*;

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, OnceLock};
    use std::time::{Duration, Instant};

    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    use crate::{config, log};

    static AS_SERVICE: AtomicBool = AtomicBool::new(false);
    static SERVICE_DIR: OnceLock<PathBuf> = OnceLock::new();

    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
    const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
    const ERROR_SERVICE_MARKED_FOR_DELETE: i32 = 1072;
    const ERROR_CANCELLED: i32 = 1223;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn running_as_service() -> bool {
        AS_SERVICE.load(Ordering::Relaxed)
    }

    fn os_error(e: &windows_service::Error) -> Option<i32> {
        match e {
            windows_service::Error::Winapi(io) => io.raw_os_error(),
            _ => None,
        }
    }

    fn describe(e: windows_service::Error) -> String {
        match os_error(&e) {
            Some(5) => "access denied (needs an administrator)".into(),
            _ => match e {
                windows_service::Error::Winapi(io) => io.to_string(),
                other => other.to_string(),
            },
        }
    }

    fn state_name(s: ServiceState) -> &'static str {
        match s {
            ServiceState::Running => "running",
            ServiceState::Stopped => "stopped",
            ServiceState::StartPending | ServiceState::ContinuePending => "starting",
            ServiceState::StopPending => "stopping",
            ServiceState::Paused | ServiceState::PausePending => "paused",
        }
    }

    pub fn status() -> Status {
        let mut st = Status { supported: true, installed: false, state: String::new(), this_is_service: running_as_service(), exe: String::new() };
        let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else { return st };
        let Ok(service) = manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG) else { return st };
        st.installed = true;
        if let Ok(s) = service.query_status() {
            st.state = state_name(s.current_state).into();
        }
        if let Ok(c) = service.query_config() {
            // The SCM keeps the whole command line: show just the program.
            let line = c.executable_path.display().to_string();
            st.exe = match line.to_lowercase().find(".exe") {
                Some(i) => line[..i + 4].trim_matches('"').to_string(),
                None => line,
            };
        }
        st
    }

    // ---- The service itself ----

    define_windows_service!(ffi_service_main, service_main);

    /// `serve --service`: hand the process to the Service Control Manager.
    pub fn run(dir: PathBuf) -> Result<(), String> {
        let _ = SERVICE_DIR.set(dir);
        AS_SERVICE.store(true, Ordering::Relaxed);
        service_dispatcher::start(NAME, ffi_service_main).map_err(|e| format!("not started by the Service Control Manager ({e}); use `serve` without --service"))
    }

    fn service_main(_args: Vec<OsString>) {
        let dir = SERVICE_DIR.get().cloned().unwrap_or_else(config::dir);
        log_to_file(&dir);
        let (stop_tx, stop_rx) = mpsc::channel();
        let handler = move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = stop_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(handle) = service_control_handler::register(NAME, handler) else { return };
        let report = |state, code: u32, wait: u64| {
            let _ = handle.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: if state == ServiceState::Running { ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN } else { ServiceControlAccept::empty() },
                exit_code: ServiceExitCode::Win32(code),
                checkpoint: 0,
                wait_hint: Duration::from_secs(wait),
                process_id: None,
            });
        };
        report(ServiceState::StartPending, 0, 40);
        let (fail_tx, fail_rx) = mpsc::channel();
        let serve_dir = dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = crate::serve(serve_dir, true) {
                log(&format!("service: {e}"));
                let _ = fail_tx.send(());
            }
        });
        report(ServiceState::Running, 0, 0);
        log("service: running");
        // Stop asked, or the server couldn't start.
        let failed = loop {
            if stop_rx.recv_timeout(Duration::from_millis(250)).is_ok() {
                break false;
            }
            if fail_rx.try_recv().is_ok() {
                break true;
            }
        };
        log(if failed { "service: stopping (failed)" } else { "service: stopping" });
        report(ServiceState::StopPending, 0, 5);
        report(ServiceState::Stopped, if failed { 1 } else { 0 }, 0);
        // Capture threads hold cards: end the process rather than unwinding them.
        std::process::exit(if failed { 1 } else { 0 });
    }

    /// The service has no console: send the log (stderr) to `capture.log`,
    /// keeping the previous one as `capture.log.old` when it gets big.
    fn log_to_file(dir: &Path) {
        use std::os::windows::io::IntoRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("capture.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 << 20) {
            let _ = std::fs::rename(&path, dir.join("capture.log.old"));
        }
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            unsafe {
                let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(file.into_raw_handle()));
            }
        }
    }

    // ---- Managing it (needs an administrator) ----

    /// `misterkvms-capture service <action>`. Errors also go to
    /// `service-error.txt`, for the copy that asked for this through UAC
    /// and can't see this process's output.
    pub fn cli(dir: &Path, action: Action) -> Result<(), String> {
        let _ = std::fs::remove_file(dir.join("service-error.txt"));
        let result = match action {
            Action::Status => {
                let s = status();
                if s.installed {
                    println!("{DISPLAY_NAME}: {} ({})", s.state, s.exe);
                } else {
                    println!("{DISPLAY_NAME}: not installed");
                }
                Ok(())
            }
            Action::Install => install(dir),
            Action::Uninstall => uninstall(dir),
            Action::Start => start(),
            Action::Stop => stop(),
            Action::Restart => stop().and_then(|_| start()),
        };
        if let Err(e) = &result {
            let _ = std::fs::write(dir.join("service-error.txt"), e);
        }
        result
    }

    fn manager() -> Result<ServiceManager, String> {
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE).map_err(describe)
    }

    fn wait_for(service: &windows_service::service::Service, wanted: ServiceState, secs: u64) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs(secs);
        loop {
            let s = service.query_status().map_err(describe)?;
            if s.current_state == wanted {
                return Ok(());
            }
            if Instant::now() > until {
                return Err(format!("the service is still {} after {secs} s", state_name(s.current_state)));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Where the service's program lives: %ProgramFiles%\MiSTerKVMS\Capture.
    fn installed_exe() -> PathBuf {
        let base = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        PathBuf::from(base).join("MiSTerKVMS").join("Capture").join("misterkvms-capture.exe")
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// Copy this program to Program Files (the service runs it from there,
    /// so the download can be moved or deleted). A newer download replaces
    /// the installed one: stop the service first so the file isn't in use.
    fn copy_program(manager: &ServiceManager) -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let target = installed_exe();
        if same_file(&exe, &target) {
            return Ok(target);
        }
        if let Ok(service) = manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP) {
            if service.stop().is_ok() {
                wait_for(&service, ServiceState::Stopped, 20)?;
            }
        }
        let folder = target.parent().expect("has a folder");
        std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        // The stopped service's process may hold the file for a moment.
        let mut tries = 0;
        loop {
            match std::fs::copy(&exe, &target) {
                Ok(_) => return Ok(target),
                Err(_) if tries < 20 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(format!("can't copy the program to {}: {e}", target.display())),
            }
        }
    }

    fn install(dir: &Path) -> Result<(), String> {
        let manager = manager()?;
        let exe = copy_program(&manager)?;
        let info = ServiceInfo {
            name: NAME.into(),
            display_name: DISPLAY_NAME.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.clone(),
            launch_arguments: vec!["serve".into(), "--service".into(), "--config".into(), dir.as_os_str().to_owned()],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let access = ServiceAccess::QUERY_STATUS | ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::STOP;
        let service = match manager.open_service(NAME, access) {
            Ok(s) => {
                s.change_config(&info).map_err(describe)?;
                s
            }
            Err(e) if os_error(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => manager.create_service(&info, access).map_err(describe)?,
            Err(e) => return Err(describe(e)),
        };
        let _ = service.set_description("Streams MiSTer capture cards to MiSTerKVMS viewers. Setup page: http://localhost:5971");
        let _ = service.update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) }; 3]),
        });
        let _ = service.set_failure_actions_on_non_crash_failures(true);
        share_config_folder(dir);
        firewall_add(dir, &exe)?;
        println!("installed {DISPLAY_NAME} ({})", exe.display());
        start()
    }

    fn uninstall(dir: &Path) -> Result<(), String> {
        let manager = manager()?;
        match manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE) {
            Ok(service) => {
                match service.stop() {
                    Ok(_) => wait_for(&service, ServiceState::Stopped, 20)?,
                    Err(e) if os_error(&e) == Some(ERROR_SERVICE_NOT_ACTIVE) => {}
                    Err(e) => return Err(describe(e)),
                }
                match service.delete() {
                    Ok(()) => {}
                    Err(e) if os_error(&e) == Some(ERROR_SERVICE_MARKED_FOR_DELETE) => {}
                    Err(e) => return Err(describe(e)),
                }
            }
            Err(e) if os_error(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {}
            Err(e) => return Err(describe(e)),
        }
        firewall_remove(dir);
        remove_program();
        println!("removed {DISPLAY_NAME}");
        Ok(())
    }

    /// Delete the Program Files copy (the config folder stays). A running
    /// program can't delete itself, so a detached cmd does it a moment later.
    fn remove_program() {
        let target = installed_exe();
        let Some(folder) = target.parent() else { return };
        if !target.exists() {
            return;
        }
        let parent = folder.parent().unwrap_or(folder);
        if std::env::current_exe().is_ok_and(|me| same_file(&me, &target)) {
            let script = format!(
                "ping -n 3 127.0.0.1 >nul & del /q \"{}\" & rmdir \"{}\" & rmdir \"{}\"",
                target.display(),
                folder.display(),
                parent.display()
            );
            let _ = std::process::Command::new("cmd").args(["/c", &script]).creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW).spawn();
        } else {
            let _ = std::fs::remove_file(&target);
            let _ = std::fs::remove_dir(folder);
            let _ = std::fs::remove_dir(parent);
        }
    }

    fn start() -> Result<(), String> {
        let manager = manager()?;
        let service = manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::START).map_err(|e| match os_error(&e) {
            Some(ERROR_SERVICE_DOES_NOT_EXIST) => "the service isn't installed".to_string(),
            _ => describe(e),
        })?;
        match service.start::<&OsStr>(&[]) {
            Ok(()) => {}
            Err(e) if os_error(&e) == Some(ERROR_SERVICE_ALREADY_RUNNING) => {}
            Err(e) => return Err(describe(e)),
        }
        wait_for(&service, ServiceState::Running, 40)?;
        println!("started {DISPLAY_NAME}");
        Ok(())
    }

    fn stop() -> Result<(), String> {
        let manager = manager()?;
        let service = manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP).map_err(|e| match os_error(&e) {
            Some(ERROR_SERVICE_DOES_NOT_EXIST) => "the service isn't installed".to_string(),
            _ => describe(e),
        })?;
        match service.stop() {
            Ok(_) => {}
            Err(e) if os_error(&e) == Some(ERROR_SERVICE_NOT_ACTIVE) => {}
            Err(e) => return Err(describe(e)),
        }
        wait_for(&service, ServiceState::Stopped, 20)?;
        println!("stopped {DISPLAY_NAME}");
        Ok(())
    }

    /// The service (LocalSystem) and a copy started by the user share the
    /// config folder: let users change files the service created.
    fn share_config_folder(dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        // S-1-5-32-545: BUILTIN\Users, by SID so it works in every language.
        let _ = std::process::Command::new("icacls")
            .arg(dir)
            .args(["/grant", "*S-1-5-32-545:(OI)(CI)M", "/T", "/Q"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }

    const RULES: [(&str, &str); 3] = [("streams", "TCP"), ("setup page", "TCP"), ("discovery", "UDP")];

    fn rule_name(what: &str) -> String {
        format!("{DISPLAY_NAME} ({what})")
    }

    fn netsh(args: &[String]) -> std::io::Result<std::process::Output> {
        std::process::Command::new("netsh").args(["advfirewall", "firewall"]).args(args).creation_flags(CREATE_NO_WINDOW).output()
    }

    fn firewall_add(dir: &Path, exe: &Path) -> Result<(), String> {
        let settings = config::load_settings(dir);
        firewall_remove(dir);
        let ports = [settings.port, settings.setup_port, 5353];
        for ((what, protocol), port) in RULES.iter().zip(ports) {
            let out = netsh(&[
                "add".into(),
                "rule".into(),
                format!("name={}", rule_name(what)),
                "dir=in".into(),
                "action=allow".into(),
                format!("program={}", exe.display()),
                format!("protocol={protocol}"),
                format!("localport={port}"),
                "profile=any".into(),
            ])
            .map_err(|e| format!("netsh: {e}"))?;
            if !out.status.success() {
                return Err(format!("firewall rule for {protocol} {port}: {}", String::from_utf8_lossy(&out.stdout).trim()));
            }
        }
        Ok(())
    }

    fn firewall_remove(_dir: &Path) {
        for (what, _) in RULES {
            let _ = netsh(&["delete".into(), "rule".into(), format!("name={}", rule_name(what))]);
        }
    }

    // ---- Asked for from the setup page ----

    /// Do `action` for the setup page. From the service itself, a helper
    /// process does it (it may stop this process). From a normal copy,
    /// Windows asks for an administrator first; returns once it's done.
    pub fn request(dir: &Path, action: Action) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if running_as_service() {
            std::process::Command::new(&exe)
                .args(["service", action.arg(), "--config"])
                .arg(dir)
                .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        let code = run_elevated(&exe, &format!("service {} --config \"{}\"", action.arg(), dir.display()))?;
        if code == 0 {
            Ok(())
        } else {
            let detail = std::fs::read_to_string(dir.join("service-error.txt")).unwrap_or_default();
            Err(if detail.trim().is_empty() { format!("failed (exit code {code})") } else { detail.trim().to_string() })
        }
    }

    /// Run `exe params` as administrator (UAC prompt) and wait for it.
    fn run_elevated(exe: &Path, params: &str) -> Result<u32, String> {
        use windows::core::{w, HSTRING};
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
        use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE, SHELLEXECUTEINFOW};
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
        use windows::Win32::UI::Shell::SEE_MASK_NOASYNC;
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        // ShellExecuteEx needs COM on the calling thread (a setup-page
        // request thread), and NOASYNC as there's no message loop here.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        }
        let file = HSTRING::from(exe.as_os_str());
        let params = HSTRING::from(params);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            // The browser the button was clicked in: without an owner in the
            // foreground, Windows only flashes the prompt in the taskbar.
            hwnd: unsafe { GetForegroundWindow() },
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: windows::core::PCWSTR(file.as_ptr()),
            lpParameters: windows::core::PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe {
            if let Err(e) = ShellExecuteExW(&mut info) {
                return Err(if e.code().0 as u32 == 0x8007_0000 | ERROR_CANCELLED as u32 { "cancelled at the Windows prompt".into() } else { e.message() });
            }
            if info.hProcess.is_invalid() {
                return Err("couldn't start the administrator helper".into());
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            Ok(code)
        }
    }

    /// Windows slows down processes it thinks are in the background (EcoQoS),
    /// which a service with no window always is: the GC573's JPEG encoding
    /// fell from 57 to 26 fps. Opt out.
    pub fn no_throttling() {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
        };
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            StateMask: 0,
        };
        unsafe {
            if let Err(e) = SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                &state as *const _ as *const _,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            ) {
                log(&format!("can't turn off power throttling: {e}"));
            }
        }
    }

    /// Double-clicked in Explorer: the console window was made for us alone.
    pub fn started_from_explorer() -> bool {
        use windows::Win32::System::Console::GetConsoleProcessList;
        let mut ids = [0u32; 4];
        unsafe { GetConsoleProcessList(&mut ids) == 1 }
    }

    pub fn open_browser(url: &str) {
        use windows::core::{w, HSTRING};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let url = HSTRING::from(url);
        unsafe {
            ShellExecuteW(None, w!("open"), &url, None, None, SW_SHOWNORMAL);
        }
    }
}
