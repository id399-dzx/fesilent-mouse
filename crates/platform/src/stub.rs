//! 通用空实现（T-004 骨架）：所有方法返回 `Unimplemented`。
//! 平台 crate 在逐能力落地前用它占位，保证 app 全链路可装配。

use crate::{
    Autostart, Button, ClipboardAccess, ClipboardContent, EventSink, HidKey, InputCapture,
    InputInject, PlatformError, Result, ScreenInfo, SecureStore, VirtualScreen,
};

fn todo<T>(what: &'static str) -> Result<T> {
    Err(PlatformError::Unimplemented(what))
}

pub struct StubCapture;
impl InputCapture for StubCapture {
    fn start(&mut self, _sink: EventSink, _grab: bool) -> Result<()> {
        todo("InputCapture::start")
    }
    fn set_grab(&mut self, _grab: bool) -> Result<()> {
        todo("InputCapture::set_grab")
    }
    fn stop(&mut self) -> Result<()> {
        todo("InputCapture::stop")
    }
    fn confine_cursor(&mut self, _on: bool) -> Result<()> {
        todo("InputCapture::confine_cursor")
    }
}

pub struct StubInject;
impl InputInject for StubInject {
    fn key(&mut self, _key: HidKey, _down: bool) -> Result<()> {
        todo("InputInject::key")
    }
    fn mouse_move_to(&mut self, _x: f64, _y: f64) -> Result<()> {
        todo("InputInject::mouse_move_to")
    }
    fn mouse_button(&mut self, _btn: Button, _down: bool) -> Result<()> {
        todo("InputInject::mouse_button")
    }
    fn scroll(&mut self, _dx: f64, _dy: f64, _precise: bool) -> Result<()> {
        todo("InputInject::scroll")
    }
    fn release_all(&mut self) -> Result<()> {
        todo("InputInject::release_all")
    }
}

pub struct StubClipboard;
impl ClipboardAccess for StubClipboard {
    fn read(&mut self) -> Result<Option<ClipboardContent>> {
        todo("ClipboardAccess::read")
    }
    fn write(&mut self, _content: &ClipboardContent) -> Result<()> {
        todo("ClipboardAccess::write")
    }
    fn change_count(&mut self) -> Result<u64> {
        todo("ClipboardAccess::change_count")
    }
}

pub struct StubScreenInfo;
impl ScreenInfo for StubScreenInfo {
    fn virtual_screen(&mut self) -> Result<VirtualScreen> {
        todo("ScreenInfo::virtual_screen")
    }
    fn keyboard_layout(&mut self) -> Result<String> {
        todo("ScreenInfo::keyboard_layout")
    }
}

pub struct StubAutostart;
impl Autostart for StubAutostart {
    fn set_enabled(&mut self, _on: bool) -> Result<()> {
        todo("Autostart::set_enabled")
    }
    fn is_enabled(&mut self) -> Result<bool> {
        todo("Autostart::is_enabled")
    }
}

pub struct StubSecureStore;
impl SecureStore for StubSecureStore {
    fn get(&mut self, _key: &str) -> Result<Option<Vec<u8>>> {
        todo("SecureStore::get")
    }
    fn set(&mut self, _key: &str, _value: &[u8]) -> Result<()> {
        todo("SecureStore::set")
    }
    fn delete(&mut self, _key: &str) -> Result<()> {
        todo("SecureStore::delete")
    }
}
