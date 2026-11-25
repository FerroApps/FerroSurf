//! Translation of winit input into Servo input events.

use servo::{
    Code, Cursor, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NamedKey, WebViewPoint, WheelDelta,
    WheelEvent, WheelMode,
};
use servo::{DevicePoint};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key as WKey, KeyLocation, ModifiersState, PhysicalKey};
use winit::window::CursorIcon;

/// Pixels per "line" for line-based wheel deltas.
pub const LINE_HEIGHT: f64 = 38.0;

pub fn modifiers(m: ModifiersState) -> Modifiers {
    let mut out = Modifiers::empty();
    out.set(Modifiers::CONTROL, m.control_key());
    out.set(Modifiers::SHIFT, m.shift_key());
    out.set(Modifiers::ALT, m.alt_key());
    out.set(Modifiers::META, m.super_key());
    out
}

pub fn keyboard_event(event: &KeyEvent, mods: ModifiersState) -> KeyboardEvent {
    let state = match event.state {
        ElementState::Pressed => KeyState::Down,
        ElementState::Released => KeyState::Up,
    };
    let key = match &event.logical_key {
        WKey::Character(c) => Key::Character(c.to_string()),
        WKey::Named(named) => match format!("{named:?}").parse::<NamedKey>() {
            Ok(k) => Key::Named(k),
            Err(_) => match named {
                winit::keyboard::NamedKey::Space => Key::Character(" ".into()),
                _ => Key::Named(NamedKey::Unidentified),
            },
        },
        _ => Key::Named(NamedKey::Unidentified),
    };
    let code = match event.physical_key {
        PhysicalKey::Code(c) => format!("{c:?}").parse::<Code>().unwrap_or(Code::Unidentified),
        PhysicalKey::Unidentified(_) => Code::Unidentified,
    };
    let location = match event.location {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    };
    KeyboardEvent::new(keyboard_types::KeyboardEvent {
        state,
        key,
        code,
        location,
        modifiers: modifiers(mods),
        repeat: event.repeat,
        is_composing: false,
    })
}

pub fn mouse_button(b: winit::event::MouseButton) -> Option<MouseButton> {
    Some(match b {
        winit::event::MouseButton::Left => MouseButton::Primary,
        winit::event::MouseButton::Right => MouseButton::Secondary,
        winit::event::MouseButton::Middle => MouseButton::Auxiliary,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        winit::event::MouseButton::Other(_) => return None,
    })
}

pub fn point(x: f32, y: f32) -> WebViewPoint {
    WebViewPoint::Device(DevicePoint::new(x, y))
}

pub fn mouse_move(x: f32, y: f32) -> InputEvent {
    InputEvent::MouseMove(MouseMoveEvent::new(point(x, y)))
}

pub fn mouse_button_event(pressed: bool, button: MouseButton, x: f32, y: f32) -> InputEvent {
    let action = if pressed { MouseButtonAction::Down } else { MouseButtonAction::Up };
    InputEvent::MouseButton(MouseButtonEvent::new(action, button, point(x, y)))
}

pub fn wheel(delta: MouseScrollDelta, x: f32, y: f32) -> InputEvent {
    let (dx, dy, mode) = match delta {
        MouseScrollDelta::LineDelta(dx, dy) => {
            (dx as f64 * LINE_HEIGHT, dy as f64 * LINE_HEIGHT, WheelMode::DeltaPixel)
        },
        MouseScrollDelta::PixelDelta(d) => (d.x, d.y, WheelMode::DeltaPixel),
    };
    // Servo expects DOM-style deltas: positive = scroll down/right.
    InputEvent::Wheel(WheelEvent::new(
        WheelDelta { x: -dx, y: -dy, z: 0.0, mode },
        point(x, y),
    ))
}

pub fn cursor_icon(cursor: Cursor) -> Option<CursorIcon> {
    Some(match cursor {
        Cursor::None => return None,
        Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
    })
}
