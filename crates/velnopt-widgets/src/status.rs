//! Text for desktop-service states, including each way a service can be unavailable.

use std::borrow::Cow;

use velnopt_services::{
    Audio, Battery, Bluetooth, Charge, Connectivity, Link, Network, Status, TrayItem,
};

/// Text for the states every service shares, or `None` when data is ready.
fn common<T>(status: &Status<T>, service: &'static str) -> Option<Cow<'static, str>> {
    Some(Cow::Owned(match status {
        Status::Pending => format!("{service}…"),
        Status::Unavailable(_) => format!("{service} unavailable"),
        Status::PermissionDenied => format!("{service}: no permission"),
        Status::Missing => format!("No {}", service.to_lowercase()),
        Status::Ready(_) => return None,
    }))
}

pub(crate) fn network(status: &Status<Network>) -> Cow<'_, str> {
    let Status::Ready(network) = status else {
        return match status {
            Status::Missing => Cow::Borrowed("No network device"),
            other => common(other, "Network").unwrap_or_default(),
        };
    };
    let link = match network.kind {
        Link::Wifi => match network.strength {
            Some(strength) => format!("Wi-Fi {} {strength}%", network.name),
            None => format!("Wi-Fi {}", network.name),
        },
        Link::Ethernet => "Ethernet".to_owned(),
        Link::Other if network.name.is_empty() => "Connected".to_owned(),
        Link::Other => network.name.clone(),
    };
    Cow::Owned(match network.state {
        Connectivity::Disconnected => return Cow::Borrowed("Offline"),
        Connectivity::Connecting => "Connecting…".to_owned(),
        Connectivity::Connected => link,
        Connectivity::Limited => format!("{link} (limited)"),
    })
}

pub(crate) fn bluetooth(status: &Status<Bluetooth>) -> Cow<'_, str> {
    let Status::Ready(bluetooth) = status else {
        return common(status, "Bluetooth").unwrap_or_default();
    };
    let state = if bluetooth.blocked {
        "Bluetooth blocked".to_owned()
    } else if !bluetooth.powered {
        "Bluetooth off".to_owned()
    } else if bluetooth.connected > 0 {
        format!("Bluetooth {}", bluetooth.connected)
    } else {
        "Bluetooth on".to_owned()
    };
    Cow::Owned(if bluetooth.control_denied {
        format!("{state} (not permitted)")
    } else {
        state
    })
}

pub(crate) fn audio(status: &Status<Audio>) -> Cow<'_, str> {
    let Status::Ready(audio) = status else {
        return match status {
            Status::Missing => Cow::Borrowed("No audio output"),
            other => common(other, "Audio").unwrap_or_default(),
        };
    };
    let state = if audio.muted {
        "Muted".to_owned()
    } else {
        format!("Volume {}%", audio.volume)
    };
    Cow::Owned(if audio.control_denied {
        format!("{state} (not permitted)")
    } else {
        state
    })
}

pub(crate) fn battery(status: &Status<Battery>) -> Cow<'_, str> {
    let Status::Ready(battery) = status else {
        return common(status, "Battery").unwrap_or_default();
    };
    Cow::Owned(match battery.state {
        Charge::Charging => format!("Battery {}% charging", battery.percent),
        Charge::Full => format!("Battery {}% full", battery.percent),
        Charge::NotCharging => format!("Battery {}% not charging", battery.percent),
        Charge::Discharging | Charge::Unknown => format!("Battery {}%", battery.percent),
    })
}

/// Text for a tray with no items to show. An empty tray draws nothing.
pub(crate) fn tray(status: &Status<Vec<TrayItem>>) -> Option<Cow<'static, str>> {
    common(status, "Tray")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_state_has_explicit_text() {
        let wifi = Network {
            state: Connectivity::Connected,
            kind: Link::Wifi,
            name: "Home".into(),
            strength: Some(72),
        };
        assert_eq!(network(&Status::Ready(wifi.clone())), "Wi-Fi Home 72%");
        let limited = Network {
            state: Connectivity::Limited,
            ..wifi.clone()
        };
        assert_eq!(network(&Status::Ready(limited)), "Wi-Fi Home 72% (limited)");
        let wired = Network {
            kind: Link::Ethernet,
            strength: None,
            ..wifi.clone()
        };
        assert_eq!(network(&Status::Ready(wired)), "Ethernet");
        let offline = Network {
            state: Connectivity::Disconnected,
            ..wifi
        };
        assert_eq!(network(&Status::Ready(offline)), "Offline");
        assert_eq!(network(&Status::Missing), "No network device");
        assert_eq!(
            network(&Status::Unavailable("no bus".into())),
            "Network unavailable"
        );
        assert_eq!(network(&Status::PermissionDenied), "Network: no permission");
        assert_eq!(network(&Status::Pending), "Network…");

        let on = Bluetooth {
            powered: true,
            blocked: false,
            connected: 2,
            control_denied: false,
        };
        assert_eq!(bluetooth(&Status::Ready(on.clone())), "Bluetooth 2");
        let denied = Bluetooth {
            powered: false,
            connected: 0,
            control_denied: true,
            ..on.clone()
        };
        assert_eq!(
            bluetooth(&Status::Ready(denied)),
            "Bluetooth off (not permitted)"
        );
        let blocked = Bluetooth {
            blocked: true,
            ..on
        };
        assert_eq!(bluetooth(&Status::Ready(blocked)), "Bluetooth blocked");
        assert_eq!(bluetooth(&Status::Missing), "No bluetooth");

        let level = Audio {
            volume: 45,
            muted: false,
            control_denied: false,
        };
        assert_eq!(audio(&Status::Ready(level.clone())), "Volume 45%");
        let muted = Audio {
            muted: true,
            ..level
        };
        assert_eq!(audio(&Status::Ready(muted)), "Muted");
        assert_eq!(audio(&Status::Missing), "No audio output");

        let charging = Battery {
            percent: 80,
            state: Charge::Charging,
        };
        assert_eq!(battery(&Status::Ready(charging)), "Battery 80% charging");
        assert_eq!(battery(&Status::Missing), "No battery");
        assert_eq!(tray(&Status::Ready(Vec::new())), None);
        assert_eq!(
            tray(&Status::Unavailable("x".into())).as_deref(),
            Some("Tray unavailable")
        );
    }
}
