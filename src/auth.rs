use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const DEFAULT_PAIRING_PIN_SECONDS: u64 = 30;

#[derive(Debug, Clone)]
pub struct PairingGate {
    secret: [u8; 16],
    step_seconds: u64,
    complete: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPin {
    pub code: String,
    pub expires_in_seconds: u64,
}

impl PairingGate {
    pub fn new(step_seconds: u64) -> Self {
        Self {
            secret: *Uuid::new_v4().as_bytes(),
            step_seconds: step_seconds.max(10),
            complete: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn current_pin(&self) -> PairingPin {
        let now = now_seconds();
        let window = now / self.step_seconds;
        let expires_in_seconds = self.step_seconds - (now % self.step_seconds);
        PairingPin {
            code: self.pin_for_window(window),
            expires_in_seconds,
        }
    }

    pub fn verify(&self, message: &str) -> bool {
        let Some(pin) = normalize_pin_message(message) else {
            return false;
        };
        pin == self.current_pin().code
    }

    pub fn mark_complete(&self) {
        self.complete.store(true, Ordering::SeqCst);
    }

    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
    }

    fn pin_for_window(&self, window: u64) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key size");
        mac.update(b"agentnoise-pairing-v1");
        mac.update(&window.to_be_bytes());
        let bytes = mac.finalize().into_bytes();
        let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
        format!("{value:06}")
    }
}

pub fn is_pairing_pin_message(message: &str) -> bool {
    normalize_pin_message(message).is_some()
}

fn normalize_pin_message(message: &str) -> Option<String> {
    let message = message.trim();
    let message = message
        .strip_prefix("/pair")
        .map(str::trim)
        .unwrap_or(message);
    let pin = message
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    (pin.len() == 6).then_some(pin)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_is_six_digits() {
        let gate = PairingGate::new(30);
        let pin = gate.current_pin();
        assert_eq!(pin.code.len(), 6);
        assert!(pin.code.chars().all(|ch| ch.is_ascii_digit()));
        assert!((1..=30).contains(&pin.expires_in_seconds));
    }

    #[test]
    fn accepts_bare_or_pair_command_pin() {
        let gate = PairingGate::new(30);
        let pin = gate.current_pin().code;
        assert!(gate.verify(&pin));
        assert!(gate.verify(&format!("/pair {pin}")));
        assert!(gate.verify(&format!("{} {}", &pin[..3], &pin[3..])));
        assert!(!gate.verify("000000"));
    }

    #[test]
    fn detects_pairing_pin_messages() {
        assert!(is_pairing_pin_message("123456"));
        assert!(is_pairing_pin_message("/pair 123456"));
        assert!(!is_pairing_pin_message("/help"));
    }
}
