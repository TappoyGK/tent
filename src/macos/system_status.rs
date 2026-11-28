use std::process::Command;

/// macOS の Wi-Fi 電波状態
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiStatus {
    Disconnected,
    Connected { bars: u8, rssi: i32 },
}

/// `system_profiler SPAirPortDataType` から現在の Wi-Fi 電波強度を取得する
pub fn read_wifi_status() -> WifiStatus {
    let Ok(out) = Command::new("system_profiler")
        .arg("SPAirPortDataType")
        .output()
    else {
        return WifiStatus::Disconnected;
    };
    if !out.status.success() {
        return WifiStatus::Disconnected;
    }
    parse_airport_wifi(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_airport_wifi(text: &str) -> WifiStatus {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Signal / Noise:") {
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(rssi) = token.parse::<i32>() {
                let bars = if rssi >= -55 {
                    4
                } else if rssi >= -67 {
                    3
                } else if rssi >= -78 {
                    2
                } else {
                    1
                };
                return WifiStatus::Connected { bars, rssi };
            }
        }
    }
    WifiStatus::Disconnected
}

/// macOS のバッテリー状態
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
}

/// `pmset -g batt` を実行して現在のバッテリー残量と充電状態を取得する
pub fn read_battery_status() -> Option<BatteryStatus> {
    let out = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_pmset_batt(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_pmset_batt(text: &str) -> Option<BatteryStatus> {
    for line in text.lines() {
        let Some(pct_pos) = line.find('%') else {
            continue;
        };
        let before = &line[..pct_pos];
        let digits: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let Ok(percent) = digits.parse::<u8>() else {
            continue;
        };
        let lower = line.to_ascii_lowercase();
        let charging = lower.contains("charging;") && !lower.contains("discharging");
        return Some(BatteryStatus {
            percent: percent.min(100),
            charging,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pmset_batt() {
        let discharging = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t84%; discharging; 5:12 remaining present: true\n";
        assert_eq!(
            parse_pmset_batt(discharging),
            Some(BatteryStatus {
                percent: 84,
                charging: false
            })
        );
        let charging = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t76%; charging; 0:45 remaining present: true\n";
        assert_eq!(
            parse_pmset_batt(charging),
            Some(BatteryStatus {
                percent: 76,
                charging: true
            })
        );
    }

    #[test]
    fn test_parse_airport_wifi() {
        let strong = "          Status: Connected\n          Signal / Noise: -52 dBm / -92 dBm\n";
        assert_eq!(
            parse_airport_wifi(strong),
            WifiStatus::Connected { bars: 4, rssi: -52 }
        );
        let fair = "          Signal / Noise: -72 dBm / -90 dBm\n";
        assert_eq!(
            parse_airport_wifi(fair),
            WifiStatus::Connected { bars: 2, rssi: -72 }
        );
        assert_eq!(
            parse_airport_wifi("Status: Off\n"),
            WifiStatus::Disconnected
        );
    }
}
