//! Test helper: press pointer buttons or scroll through wlr-virtual-pointer.
//!
//! `scripts/smoke-hyprland.py` runs it inside a disposable nested Hyprland, after
//! `hyprctl dispatch movecursor`. Never point it at a real desktop session.
//!
//! Usage: `virtual-pointer click`, `virtual-pointer scroll <steps>`, or
//! `virtual-pointer hold`. `hold` keeps a pointer device present until it is killed,
//! the way a mouse does on a desktop (a headless parent compositor has none).

use smithay_client_toolkit::reexports::{
    client::{
        Connection, QueueHandle, delegate_noop,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_pointer, wl_registry},
    },
    protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    },
};

const BUTTON_LEFT: u32 = 0x110;

struct State;

impl
    smithay_client_toolkit::reexports::client::Dispatch<wl_registry::WlRegistry, GlobalListContents>
    for State
{
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
    let qh = queue.handle();
    let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ())?;
    let pointer = manager.create_virtual_pointer(None, &qh, ());
    match arguments.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["click"] => {
            for state in [
                wl_pointer::ButtonState::Pressed,
                wl_pointer::ButtonState::Released,
            ] {
                pointer.button(0, BUTTON_LEFT, state);
                pointer.frame();
            }
        }
        ["scroll", steps] => {
            let steps: i32 = steps.parse()?;
            pointer.axis_source(wl_pointer::AxisSource::Wheel);
            pointer.axis_discrete(
                0,
                wl_pointer::Axis::VerticalScroll,
                f64::from(steps) * 15.0,
                steps,
            );
            pointer.frame();
        }
        ["hold"] => loop {
            queue.blocking_dispatch(&mut State)?;
        },
        _ => return Err("usage: virtual-pointer click | scroll <steps> | hold".into()),
    }
    pointer.destroy();
    queue.roundtrip(&mut State)?;
    Ok(())
}
