use std::time::{SystemTime, UNIX_EPOCH};

/// Wall clock in 100 ns units since the Unix epoch (the timestamp unit used
/// everywhere in MiSTerKVMS).
pub fn now_100ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| (d.as_nanos() / 100) as i64)
}

/// Turns a device's own timestamps (arbitrary origin) into wall-clock ones
/// without adding the jitter of "time of arrival": the offset is taken from
/// the first sample and re-taken if the two drift apart.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
#[derive(Default)]
pub(crate) struct DeviceClock {
    offset: Option<i64>,
}

#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
impl DeviceClock {
    pub(crate) fn wall(&mut self, device_ts: i64) -> i64 {
        let now = now_100ns();
        let offset = *self.offset.get_or_insert(now - device_ts);
        let wall = device_ts + offset;
        // More than 200 ms apart (device restarted, clock jump): re-anchor.
        if (wall - now).abs() > 2_000_000 {
            self.offset = Some(now - device_ts);
            return now;
        }
        wall
    }
}
