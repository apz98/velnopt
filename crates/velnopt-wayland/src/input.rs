//! Pointer clicks and keyboard navigation for panel controls.
//!
//! An idle panel asks for no keyboard focus, so clicking a control never steals
//! focus from the user's window. Keyboard mode (SIGUSR1, or a focus request from
//! assistive technology) asks for exclusive focus until Escape or focus loss.

use smithay_client_toolkit::{
    reexports::client::{
        Connection, QueueHandle,
        protocol::{wl_keyboard, wl_pointer, wl_seat, wl_surface},
    },
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::WaylandSurface,
};

use velnopt_services::Request;
use velnopt_widgets::Action;

use crate::{App, VOLUME_STEP};

/// Linux input event codes for the primary and secondary pointer buttons.
const BUTTON_LEFT: u32 = 0x110;
const BUTTON_RIGHT: u32 = 0x111;

/// A navigation or activation request decoded from a single key press.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Key {
    Next,
    Previous,
    Up,
    Down,
    First,
    Last,
    Activate,
    Leave,
}

impl Key {
    /// Map layout-independent keysyms. Other keys are ignored in keyboard mode.
    pub(crate) fn from_keysym(keysym: Keysym, shift: bool) -> Option<Self> {
        Some(match keysym {
            Keysym::Tab if shift => Self::Previous,
            Keysym::Right | Keysym::Tab | Keysym::KP_Right => Self::Next,
            Keysym::Left | Keysym::ISO_Left_Tab | Keysym::KP_Left => Self::Previous,
            Keysym::Up | Keysym::KP_Up => Self::Up,
            Keysym::Down | Keysym::KP_Down => Self::Down,
            Keysym::Home | Keysym::KP_Home => Self::First,
            Keysym::End | Keysym::KP_End => Self::Last,
            Keysym::Return | Keysym::KP_Enter | Keysym::space => Self::Activate,
            Keysym::Escape => Self::Leave,
            _ => return None,
        })
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                match self.seat.get_keyboard(qh, &seat, None) {
                    Ok(keyboard) => self.keyboard = Some(keyboard),
                    Err(error) => eprintln!("keyboard input unavailable: {error}"),
                }
            }
            Capability::Pointer if self.pointer.is_none() => {
                match self.seat.get_pointer(qh, &seat) {
                    Ok(pointer) => self.pointer = Some(pointer),
                    Err(error) => eprintln!("pointer input unavailable: {error}"),
                }
            }
            _ => {}
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
                self.leave_keyboard_mode(None);
            }
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            _ => {}
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self
                .panels
                .iter()
                .position(|panel| panel.layer.wl_surface() == &event.surface)
            else {
                continue;
            };
            let panel = &mut self.panels[index];
            let x = (event.position.0 * f64::from(panel.scale)) as i32;
            let Some(action) = panel
                .hits
                .iter()
                .find(|(span, _)| (span.start..span.end).contains(&x))
                .map(|(_, action)| *action)
            else {
                continue;
            };
            match event.kind {
                PointerEventKind::Press {
                    button: BUTTON_LEFT,
                    ..
                } => {
                    if panel.keyboard_mode {
                        panel.focus = Some(action);
                        panel.dirty = true;
                    }
                    self.activate(index, action);
                }
                PointerEventKind::Press {
                    button: BUTTON_RIGHT,
                    ..
                } => {
                    if let Action::Tray(item) = action {
                        self.tray(index, item, true);
                    }
                }
                PointerEventKind::Axis { vertical, .. } if action == Action::ToggleMute => {
                    // wheel up (negative) raises the volume
                    let direction = if vertical.discrete != 0 {
                        -vertical.discrete.signum()
                    } else {
                        -(vertical.absolute.signum() as i32)
                    };
                    if direction != 0 {
                        self.request(Request::AdjustVolume(direction as i8 * VOLUME_STEP));
                    }
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        if let Some(panel) = self
            .panels
            .iter_mut()
            .find(|panel| panel.layer.wl_surface() == surface)
        {
            panel.keyboard_focus = true;
            if let Some(access) = &mut panel.access {
                access.set_focused(true);
            }
        }
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.leave_keyboard_mode(Some(surface.clone()));
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.launcher_text(event.keysym, event.utf8.as_deref()) {
            return;
        }
        if let Some(key) = Key::from_keysym(event.keysym, self.shift) {
            self.key(key);
        }
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.launcher_text(event.keysym, event.utf8.as_deref()) {
            return;
        }
        // a held Enter would fire the action over and over, so repeat only navigation and adjustment
        if let Some(key @ (Key::Next | Key::Previous | Key::Up | Key::Down)) =
            Key::from_keysym(event.keysym, self.shift)
        {
            self.key(key);
        }
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.shift = modifiers.shift;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_keys_are_layout_independent_and_shift_tab_reverses() {
        assert_eq!(Key::from_keysym(Keysym::Tab, false), Some(Key::Next));
        assert_eq!(Key::from_keysym(Keysym::Tab, true), Some(Key::Previous));
        assert_eq!(
            Key::from_keysym(Keysym::ISO_Left_Tab, true),
            Some(Key::Previous)
        );
        assert_eq!(Key::from_keysym(Keysym::Right, false), Some(Key::Next));
        assert_eq!(Key::from_keysym(Keysym::Left, false), Some(Key::Previous));
        assert_eq!(Key::from_keysym(Keysym::Up, false), Some(Key::Up));
        assert_eq!(Key::from_keysym(Keysym::KP_Down, false), Some(Key::Down));
        assert_eq!(Key::from_keysym(Keysym::Home, false), Some(Key::First));
        assert_eq!(Key::from_keysym(Keysym::End, false), Some(Key::Last));
        for key in [Keysym::Return, Keysym::KP_Enter, Keysym::space] {
            assert_eq!(Key::from_keysym(key, false), Some(Key::Activate));
        }
        assert_eq!(Key::from_keysym(Keysym::Escape, false), Some(Key::Leave));
        assert_eq!(Key::from_keysym(Keysym::a, false), None);
    }
}
