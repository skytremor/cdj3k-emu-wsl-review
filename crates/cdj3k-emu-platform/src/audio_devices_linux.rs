//! PulseAudio/PipeWire sink enumeration for the Linux menu.

#![cfg(target_os = "linux")]

use std::process::Command;

use crate::menu_state::AudioOutDevice;

pub fn enumerate_output_devices() -> Vec<AudioOutDevice> {
    let Ok(output) = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _index = fields.next()?;
            let name = fields.next()?.to_string();
            Some(AudioOutDevice {
                uid: name.clone(),
                name,
                is_default: false,
                sample_rate_hz: 0,
            })
        })
        .collect()
}
