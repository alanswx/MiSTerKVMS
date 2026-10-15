//! A small C interface to the `Controller`, for shells that can't use the
//! UniFFI bindings (the Windows C# app P/Invokes it). Structured values go
//! across as JSON, so there are only a few functions:
//!
//! ```c
//! typedef void (*kvms_event_fn)(void *user, const char *event);
//! void *kvms_controller_new(kvms_event_fn on_event, void *user);
//! void  kvms_controller_free(void *controller);
//! char *kvms_call(void *controller, const char *request_json);
//! void  kvms_string_free(char *s);
//! ```
//!
//! A request is `{"cmd": "<name>", ...arguments}`. The reply is
//! `{"ok": <value>}` or `{"error": "<message>"}`. Events are the
//! `ControllerEvent` names ("changed", "profilesChanged", ...), sent from
//! background threads.

use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::controller::{Controller, ControllerEvent, ControllerObserver};
use crate::profiles::HostProfile;
use crate::video::{DisplaySettings, NativeSurface};

pub type EventFn = extern "C" fn(user: *mut c_void, event: *const c_char);

struct CObserver {
    on_event: EventFn,
    user: usize,
}

impl ControllerObserver for CObserver {
    fn on_event(&self, event: ControllerEvent) {
        let name = match event {
            ControllerEvent::Changed => c"changed",
            ControllerEvent::ProfilesChanged => c"profilesChanged",
            ControllerEvent::CaptureRequested => c"captureRequested",
            ControllerEvent::ReleaseRequested => c"releaseRequested",
        };
        (self.on_event)(self.user as *mut c_void, name.as_ptr());
    }
}

/// Create the controller (settings in the platform config folder). Free it
/// with `kvms_controller_free`.
#[unsafe(no_mangle)]
pub extern "C" fn kvms_controller_new(on_event: EventFn, user: *mut c_void) -> *mut c_void {
    let observer = Arc::new(CObserver { on_event, user: user as usize });
    Box::into_raw(Box::new(Controller::new(observer))) as *mut c_void
}

/// # Safety
/// `controller` must come from `kvms_controller_new` and not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kvms_controller_free(controller: *mut c_void) {
    if !controller.is_null() {
        // SAFETY: per the contract above.
        drop(unsafe { Box::from_raw(controller as *mut Arc<Controller>) });
    }
}

/// Run one request. The reply must be freed with `kvms_string_free`.
///
/// # Safety
/// `controller` must come from `kvms_controller_new`; `request` must be a
/// NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kvms_call(controller: *mut c_void, request: *const c_char) -> *mut c_char {
    let reply = catch_unwind(AssertUnwindSafe(|| {
        if controller.is_null() || request.is_null() {
            return Err("null argument".to_string());
        }
        // SAFETY: per the contract above.
        let controller = unsafe { &*(controller as *const Arc<Controller>) };
        let request = unsafe { CStr::from_ptr(request) }.to_str().map_err(|e| e.to_string())?;
        let request: Value = serde_json::from_str(request).map_err(|e| format!("bad request: {e}"))?;
        call(controller, request)
    }))
    .unwrap_or_else(|_| Err("internal error (see log)".into()));
    let reply = match reply {
        Ok(value) => json!({ "ok": value }),
        Err(message) => json!({ "error": message }),
    };
    CString::new(reply.to_string()).unwrap_or_default().into_raw()
}

/// # Safety
/// `s` must come from `kvms_call` and not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kvms_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: per the contract above.
        drop(unsafe { CString::from_raw(s) });
    }
}

fn arg<T: for<'de> Deserialize<'de>>(request: &Value, name: &str) -> Result<T, String> {
    let value = request.get(name).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|e| format!("argument '{name}': {e}"))
}

fn to_json<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn call(c: &Arc<Controller>, r: Value) -> Result<Value, String> {
    let cmd: String = arg(&r, "cmd")?;
    let done = Ok(Value::Null);
    match cmd.as_str() {
        "version" => Ok(json!(env!("CARGO_PKG_VERSION"))),
        "ffmpegAvailable" => Ok(json!(crate::video::ffmpeg::ffmpeg_available())),
        // Capture server streams on the network; blocks up to waitMs.
        "findStreams" => {
            let wait: Option<u32> = arg(&r, "waitMs")?;
            to_json(crate::discover::find_streams(wait.unwrap_or(2000)))
        }
        "captureCards" => to_json(crate::discover::capture_cards()),
        "audioInputs" => to_json(crate::discover::audio_inputs()),
        "takeControl" => {
            c.take_control();
            done
        }
        "viewerName" => Ok(json!(c.viewer_name())),
        "setViewerName" => {
            c.set_viewer_name(arg(&r, "name")?);
            done
        }
        "setQuality" => {
            c.set_quality(arg(&r, "quality")?);
            done
        }

        "profiles" => to_json(c.profiles()),
        "newProfile" => to_json(HostProfile::default()),
        "saveProfile" => {
            c.save_profile(arg(&r, "profile")?);
            done
        }
        "deleteProfile" => {
            c.delete_profile(arg(&r, "id")?);
            done
        }
        "exportProfiles" => Ok(json!(c.export_profiles())),
        "importProfiles" => c.import_profiles(arg(&r, "json")?).map(|n| json!(n)).map_err(|e| e.to_string()),
        "settingsPath" => Ok(json!(c.settings_path())),

        "open" => {
            c.open(arg(&r, "id")?);
            done
        }
        "close" => {
            c.close();
            done
        }
        "state" => to_json(c.state()),

        "attachSurface" => {
            let surface: NativeSurface = arg(&r, "surface")?;
            c.attach_surface(surface, arg(&r, "width")?, arg(&r, "height")?, arg(&r, "scale")?);
            done
        }
        "detachSurface" => {
            c.detach_surface();
            done
        }
        "resizeSurface" => {
            c.resize_surface(arg(&r, "width")?, arg(&r, "height")?, arg(&r, "scale")?);
            done
        }
        #[cfg(windows)]
        "createCompositionSurface" => crate::win::create_composition_surface().map(|h| json!(h)),
        #[cfg(windows)]
        "closeCompositionSurface" => {
            crate::win::close_composition_surface(arg(&r, "handle")?);
            done
        }

        #[cfg(windows)]
        "startCapture" => {
            let window: i64 = arg(&r, "window")?;
            c.start_capture(window as isize, arg(&r, "x")?, arg(&r, "y")?, arg(&r, "width")?, arg(&r, "height")?)
                .map(|_| Value::Null)
        }
        #[cfg(windows)]
        "stopCapture" => {
            c.stop_capture();
            done
        }
        "setCapturing" => {
            c.set_capturing(arg(&r, "capturing")?);
            done
        }
        "openMenu" => {
            c.open_menu();
            done
        }
        "ctrlAltDel" => {
            c.send_ctrl_alt_del();
            done
        }
        "releaseAll" => {
            c.release_all();
            done
        }

        "setDisplay" => {
            let display: DisplaySettings = arg(&r, "display")?;
            c.set_display(display);
            done
        }
        "zoomBy" => {
            c.zoom_by(arg(&r, "factor")?);
            done
        }
        "resetZoom" => {
            c.reset_zoom();
            done
        }
        "panBy" => {
            c.pan_by(arg(&r, "dx")?, arg(&r, "dy")?);
            done
        }
        "setMuted" => {
            c.set_muted(arg(&r, "muted")?);
            done
        }
        "flashStats" => {
            c.flash_stats();
            done
        }
        "setStatsPinned" => {
            c.set_stats_pinned(arg(&r, "pinned")?);
            done
        }
        "startRecording" => c.start_recording(arg(&r, "path")?).map(|_| Value::Null).map_err(|e| e.to_string()),
        "stopRecording" => {
            c.stop_recording();
            done
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn ignore(_: *mut c_void, _: *const c_char) {}

    fn request(c: *mut c_void, json: &str) -> Value {
        let req = CString::new(json).unwrap();
        // SAFETY: valid controller and request.
        let reply = unsafe { kvms_call(c, req.as_ptr()) };
        let text = unsafe { CStr::from_ptr(reply) }.to_str().unwrap().to_string();
        unsafe { kvms_string_free(reply) };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn json_round_trip() {
        let observer = Arc::new(CObserver { on_event: ignore, user: 0 });
        let path = std::env::temp_dir().join(format!("kvms-capi-{}.json", std::process::id()));
        let c = Box::into_raw(Box::new(Controller::with_settings_file(path.display().to_string(), observer))) as *mut c_void;

        let profile = request(c, r#"{"cmd":"newProfile"}"#)["ok"].clone();
        assert_eq!(profile["misterPort"], 8182);
        let id = profile["id"].as_str().unwrap().to_string();
        let mut edited = profile.clone();
        edited["name"] = json!("Test");
        request(c, &json!({"cmd": "saveProfile", "profile": edited}).to_string());
        assert_eq!(request(c, r#"{"cmd":"profiles"}"#)["ok"][0]["name"], "Test");

        request(c, &json!({"cmd": "open", "id": id}).to_string());
        let state = request(c, r#"{"cmd":"state"}"#)["ok"].clone();
        assert_eq!(state["profileId"], json!(id));
        assert_eq!(state["connection"]["state"], "disconnected");
        assert_eq!(state["display"]["scaleMode"], "fit");

        let mut display = state["display"].clone();
        display["scaleMode"] = json!("integer");
        request(c, &json!({"cmd": "setDisplay", "display": display}).to_string());
        assert_eq!(request(c, r#"{"cmd":"state"}"#)["ok"]["display"]["scaleMode"], "integer");

        assert!(request(c, r#"{"cmd":"nope"}"#)["error"].is_string());
        assert!(request(c, r#"{"cmd":"zoomBy"}"#)["error"].as_str().unwrap().contains("factor"));
        unsafe { kvms_controller_free(c) };
        let _ = std::fs::remove_file(path);
    }
}
