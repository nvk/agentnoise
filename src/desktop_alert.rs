use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use qrcode::{Color, QrCode};

use crate::auth::PairingPin;

#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Child, Command, Stdio};

pub struct AlertHandle {
    #[cfg(target_os = "macos")]
    child: Child,
    #[cfg(target_os = "macos")]
    qr_path: PathBuf,
    #[cfg(target_os = "macos")]
    cleaned_up: bool,
}

impl AlertHandle {
    pub fn has_exited(&mut self) -> bool {
        let exited = self.try_wait().unwrap_or(true);
        if exited {
            self.cleanup();
        }
        exited
    }

    pub fn close(&mut self) {
        self.close_inner();
        self.cleanup();
    }

    #[cfg(target_os = "macos")]
    fn try_wait(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    #[cfg(not(target_os = "macos"))]
    fn try_wait(&mut self) -> Result<bool> {
        Ok(true)
    }

    #[cfg(target_os = "macos")]
    fn close_inner(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        self.child.kill().ok();
        self.child.wait().ok();
    }

    #[cfg(not(target_os = "macos"))]
    fn close_inner(&mut self) {}

    #[cfg(target_os = "macos")]
    fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        fs::remove_file(&self.qr_path).ok();
        self.cleaned_up = true;
    }

    #[cfg(not(target_os = "macos"))]
    fn cleanup(&mut self) {}
}

impl Drop for AlertHandle {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn spawn_pairing_pin_alert(
    pin: &PairingPin,
    npub: &str,
    nprofile: &str,
) -> Result<Option<AlertHandle>> {
    platform_spawn_pairing_pin_alert(pin, npub, nprofile)
}

pub fn show_pairing_success_alert() -> Result<()> {
    platform_show_pairing_success_alert()
}

#[cfg(target_os = "macos")]
fn platform_spawn_pairing_pin_alert(
    pin: &PairingPin,
    npub: &str,
    nprofile: &str,
) -> Result<Option<AlertHandle>> {
    let seconds = pin.expires_in_seconds.max(1);
    let qr_path = pairing_qr_path(&pin.code);
    write_qr_bmp(nprofile, &qr_path)?;
    let child = Command::new("osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(PAIRING_WINDOW_JXA)
        .env("AGENTNOISE_PIN", &pin.code)
        .env("AGENTNOISE_SECONDS", seconds.to_string())
        .env("AGENTNOISE_NPUB", npub)
        .env("AGENTNOISE_NPROFILE", nprofile)
        .env("AGENTNOISE_QR_PATH", &qr_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("showing macOS AgentNoise pairing alert")?;
    Ok(Some(AlertHandle {
        child,
        qr_path,
        cleaned_up: false,
    }))
}

#[cfg(not(target_os = "macos"))]
fn platform_spawn_pairing_pin_alert(
    _pin: &PairingPin,
    _npub: &str,
    _nprofile: &str,
) -> Result<Option<AlertHandle>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn platform_show_pairing_success_alert() -> Result<()> {
    let script = format!(
        "display dialog {} buttons {{\"OK\"}} default button \"OK\" with title \"AgentNoise Pairing\"",
        applescript_string("AgentNoise paired.\n\nSend /help from White Noise.")
    );
    let mut handle = spawn_osascript(script)?;
    handle.child.wait().ok();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn platform_show_pairing_success_alert() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn spawn_osascript(script: String) -> Result<AlertHandle> {
    let child = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("showing macOS AgentNoise pairing alert")?;
    Ok(AlertHandle {
        child,
        qr_path: PathBuf::new(),
        cleaned_up: true,
    })
}

#[cfg(target_os = "macos")]
fn applescript_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(target_os = "macos")]
const PAIRING_WINDOW_JXA: &str = r#"
ObjC.import('Cocoa');

function env(name) {
  const value = $.NSProcessInfo.processInfo.environment.objectForKey(name);
  return value ? ObjC.unwrap(value) : '';
}

const pin = env('AGENTNOISE_PIN');
const seconds = parseInt(env('AGENTNOISE_SECONDS'), 10) || 30;
const npub = env('AGENTNOISE_NPUB');
const qrPath = env('AGENTNOISE_QR_PATH');

const app = $.NSApplication.sharedApplication;
app.setActivationPolicy($.NSApplicationActivationPolicyAccessory);

const windowWidth = 430;
const windowHeight = 560;
const style = $.NSWindowStyleMaskTitled | $.NSWindowStyleMaskClosable;
const window = $.NSWindow.alloc.initWithContentRectStyleMaskBackingDefer(
  $.NSMakeRect(0, 0, windowWidth, windowHeight),
  style,
  $.NSBackingStoreBuffered,
  false
);
window.setTitle('AgentNoise Pairing');
window.setReleasedWhenClosed(false);

const content = window.contentView;

function label(text, x, y, width, height, size, bold) {
  const field = $.NSTextField.alloc.initWithFrame($.NSMakeRect(x, y, width, height));
  field.setEditable(false);
  field.setSelectable(false);
  field.setBezeled(false);
  field.setDrawsBackground(false);
  field.setStringValue(text);
  field.setAlignment($.NSTextAlignmentCenter);
  field.setLineBreakMode($.NSLineBreakByWordWrapping);
  field.setFont(bold ? $.NSFont.boldSystemFontOfSize(size) : $.NSFont.systemFontOfSize(size));
  content.addSubview(field);
  return field;
}

label('Pair AgentNoise', 20, 516, 390, 28, 22, true);
label('Scan this desktop identity in White Noise, then send the PIN.', 35, 486, 360, 24, 13, false);

const image = $.NSImage.alloc.initWithContentsOfFile(qrPath);
const imageView = $.NSImageView.alloc.initWithFrame($.NSMakeRect(90, 230, 250, 250));
imageView.setImage(image);
imageView.setImageScaling($.NSImageScaleProportionallyUpOrDown);
content.addSubview(imageView);

label('PIN', 20, 190, 390, 18, 12, false);
label(pin, 20, 150, 390, 44, 38, true);
const countdown = label('', 20, 124, 390, 24, 14, false);

const npubField = label(npub, 25, 82, 380, 34, 10, false);
npubField.setSelectable(true);

const closeButton = $.NSButton.alloc.initWithFrame($.NSMakeRect(170, 34, 90, 32));
closeButton.setTitle('Close');
closeButton.setBezelStyle($.NSBezelStyleRounded);
closeButton.setTarget(app);
closeButton.setAction('terminate:');
content.addSubview(closeButton);

window.center;
window.makeKeyAndOrderFront(null);
app.activateIgnoringOtherApps(true);

const deadline = Date.now() + (seconds * 1000);
let lastLeft = -1;
while (Date.now() < deadline && window.isVisible) {
  const left = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
  if (left !== lastLeft) {
    countdown.setStringValue('Expires in ' + left + ' seconds');
    lastLeft = left;
  }
  const event = app.nextEventMatchingMaskUntilDateInModeDequeue(
    $.NSEventMaskAny,
    $.NSDate.dateWithTimeIntervalSinceNow(0.2),
    $.NSDefaultRunLoopMode,
    true
  );
  if (event) {
    app.sendEvent(event);
    app.updateWindows;
  }
}

window.close;
"#;

fn write_qr_bmp(payload: &str, path: &Path) -> Result<()> {
    let code = QrCode::new(payload.as_bytes()).context("building QR code")?;
    let quiet_zone = 4usize;
    let modules = code.width() + (quiet_zone * 2);
    let module_pixels = (320 / modules).clamp(3, 8);
    let image_width = modules * module_pixels;
    let image_height = image_width;
    let row_stride = (image_width * 3).next_multiple_of(4);
    let pixel_data_size = row_stride * image_height;
    let file_size = 14 + 40 + pixel_data_size;

    let mut bytes = Vec::with_capacity(file_size);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&(file_size as u32).to_le_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&(54u32).to_le_bytes());
    bytes.extend_from_slice(&(40u32).to_le_bytes());
    bytes.extend_from_slice(&(image_width as i32).to_le_bytes());
    bytes.extend_from_slice(&(image_height as i32).to_le_bytes());
    bytes.extend_from_slice(&(1u16).to_le_bytes());
    bytes.extend_from_slice(&(24u16).to_le_bytes());
    bytes.extend_from_slice(&(0u32).to_le_bytes());
    bytes.extend_from_slice(&(pixel_data_size as u32).to_le_bytes());
    bytes.extend_from_slice(&(2835u32).to_le_bytes());
    bytes.extend_from_slice(&(2835u32).to_le_bytes());
    bytes.extend_from_slice(&(0u32).to_le_bytes());
    bytes.extend_from_slice(&(0u32).to_le_bytes());

    for y in (0..image_height).rev() {
        for x in 0..image_width {
            let cell_x = x / module_pixels;
            let cell_y = y / module_pixels;
            let dark = cell_x >= quiet_zone
                && cell_x < quiet_zone + code.width()
                && cell_y >= quiet_zone
                && cell_y < quiet_zone + code.width()
                && code[(cell_x - quiet_zone, cell_y - quiet_zone)] != Color::Light;
            if dark {
                bytes.extend_from_slice(&[0, 0, 0]);
            } else {
                bytes.extend_from_slice(&[255, 255, 255]);
            }
        }
        bytes.resize(bytes.len() + (row_stride - image_width * 3), 0);
    }

    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

#[cfg(target_os = "macos")]
fn pairing_qr_path(pin: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "agentnoise-pairing-{}-{pin}.bmp",
        std::process::id()
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn quotes_applescript_strings() {
        assert_eq!(applescript_string("a \"quote\""), "\"a \\\"quote\\\"\"");
        assert_eq!(applescript_string("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn writes_qr_bmp() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("qr.bmp");
        write_qr_bmp("nprofile1test", &path).unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[..2], b"BM");
        assert!(bytes.len() > 54);
    }
}
