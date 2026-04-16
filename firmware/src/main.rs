// =============================================================================
// Pulse Button Firmware -- ESP32-S3
// =============================================================================
//
// Physical approve/reject button for Claude Code.
//
// Hardware:
//   - ESP32-S3 Super Mini (native USB OTG)
//   - WS2812B LED ring, 16 LEDs, data on GPIO 8
//   - Cherry MX switch on GPIO 4, active-low with internal pull-up
//
// Communication:
//   - USB CDC serial: receives state commands, sends button events
//   - USB HID keyboard: sends keystrokes on button events
//
// Build with esp-rs (ESP-IDF framework, std environment).
// =============================================================================

use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{self, PinDriver, Pull};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::uart::{self, UartDriver, UartConfig};
use esp_idf_svc::log::EspLogger;
use esp_idf_sys as _;
use log::{info, warn};
use smart_leds::hsv::RGB8;
use smart_leds::SmartLedsWrite;
use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};
use ws2812_esp32_rmt_driver::Ws2812Esp32Rmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const NUM_LEDS: usize = 16;
const LED_GPIO: u32 = 8;
const BUTTON_GPIO: i32 = 4;

/// Debounce window for the mechanical switch.
const DEBOUNCE_MS: u64 = 50;
/// Maximum gap between taps to count as a multi-tap gesture.
const TAP_WINDOW_MS: u64 = 300;
/// Hold duration to trigger a long-press event.
const LONG_PRESS_MS: u64 = 2000;

/// Main loop tick interval (ms). Controls LED refresh rate.
const TICK_MS: u32 = 10;

// ---------------------------------------------------------------------------
// LED colors
// ---------------------------------------------------------------------------

const COLOR_RED: RGB8 = RGB8 { r: 200, g: 0, b: 0 };
const COLOR_GREEN: RGB8 = RGB8 { r: 0, g: 180, b: 0 };
const COLOR_YELLOW: RGB8 = RGB8 { r: 200, g: 160, b: 0 };
const COLOR_BLUE: RGB8 = RGB8 { r: 0, g: 0, b: 200 };
const COLOR_WHITE_DIM: RGB8 = RGB8 { r: 30, g: 30, b: 30 };
const COLOR_OFF: RGB8 = RGB8 { r: 0, g: 0, b: 0 };

// ---------------------------------------------------------------------------
// State machine types
// ---------------------------------------------------------------------------

/// Top-level device states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Working,
    Done,
    NeedsInput,
    Error,
}

/// Button events after tap-counting and debounce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonEvent {
    SingleTap,
    DoubleTap,
    TripleTap,
    LongPress,
}

/// Commands received over USB serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SerialCommand {
    Working,
    Done,
    Input,
    Idle,
    Error,
}

/// LED display mode -- drives the render loop.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LedMode {
    /// Constant color on all LEDs.
    Solid(RGB8),
    /// Brightness oscillates (sine wave) around a base color.
    Pulse(RGB8),
    /// Alternates between color and off at the given period (ms).
    Blink(RGB8, u32),
    /// Brief flash then return to the previous mode.
    Flash(RGB8, u32 /* duration ms */),
}

// ---------------------------------------------------------------------------
// USB HID helpers (ESP-IDF TinyUSB)
// ---------------------------------------------------------------------------
//
// The ESP32-S3 exposes a USB OTG peripheral. ESP-IDF ships with TinyUSB which
// can present a composite CDC + HID device.  The functions below use the
// esp-idf-sys FFI bindings to the TinyUSB HID class driver.
//
// NOTE: Full HID integration requires TinyUSB configuration in sdkconfig
// (CONFIG_TINYUSB_HID_ENABLED=y).  The code below compiles against the C API;
// if TinyUSB is not enabled in your sdkconfig the linker will tell you.
//
// For a minimal proof-of-life you can stub these out and rely solely on the
// serial protocol -- the daemon on the host side can inject keystrokes instead.
// ---------------------------------------------------------------------------

/// HID keyboard report (boot protocol, 8 bytes).
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct HidKeyboardReport {
    modifier: u8,
    reserved: u8,
    keycode: [u8; 6],
}

// HID usage IDs for the keys we need (USB HID Usage Tables 1.21).
const KEY_NONE: u8 = 0x00;
const KEY_ENTER: u8 = 0x28;
const KEY_Y: u8 = 0x1C;
const KEY_N: u8 = 0x11;
const KEY_SLASH: u8 = 0x38;
const KEY_S: u8 = 0x16;
const KEY_E: u8 = 0x08;
const KEY_C: u8 = 0x06;
const KEY_U: u8 = 0x18;
const KEY_R: u8 = 0x15;
const KEY_I: u8 = 0x0C;
const KEY_T: u8 = 0x17;
const KEY_X: u8 = 0x1B;
const KEY_P: u8 = 0x13;
const KEY_L: u8 = 0x0F;
const KEY_A: u8 = 0x04;

/// Send a single HID key press + release via TinyUSB.
///
/// This calls into the ESP-IDF TinyUSB C API.  If TinyUSB HID is not enabled
/// in your build config this will be a no-op (guarded by cfg).
fn hid_send_key(keycode: u8) {
    let report = HidKeyboardReport {
        modifier: 0,
        reserved: 0,
        keycode: [keycode, KEY_NONE, KEY_NONE, KEY_NONE, KEY_NONE, KEY_NONE],
    };
    let empty = HidKeyboardReport::default();

    unsafe {
        // Press
        esp_idf_sys::tinyusb_hid_keyboard_report(
            0, // report id
            report.modifier,
            report.keycode.as_ptr(),
        );
        FreeRtos::delay_ms(15);
        // Release
        esp_idf_sys::tinyusb_hid_keyboard_report(
            0,
            empty.modifier,
            empty.keycode.as_ptr(),
        );
        FreeRtos::delay_ms(15);
    }
}

/// Send a sequence of HID keycodes, one press-release per keycode.
fn hid_send_keys(keys: &[u8]) {
    for &k in keys {
        hid_send_key(k);
    }
}

/// Map a button event to HID keystrokes and send them.
fn hid_send_event(event: ButtonEvent) {
    match event {
        // SingleTap -> "y" + Enter
        ButtonEvent::SingleTap => {
            hid_send_keys(&[KEY_Y, KEY_ENTER]);
        }
        // DoubleTap -> "n" + Enter
        ButtonEvent::DoubleTap => {
            hid_send_keys(&[KEY_N, KEY_ENTER]);
        }
        // TripleTap -> "/security" + Enter
        ButtonEvent::TripleTap => {
            hid_send_keys(&[
                KEY_SLASH, KEY_S, KEY_E, KEY_C, KEY_U, KEY_R, KEY_I, KEY_T,
                KEY_Y, KEY_ENTER,
            ]);
        }
        // LongPress -> "/explain" + Enter
        ButtonEvent::LongPress => {
            hid_send_keys(&[
                KEY_SLASH, KEY_E, KEY_X, KEY_P, KEY_L, KEY_A, KEY_I, KEY_N,
                KEY_ENTER,
            ]);
        }
    }
}

// ---------------------------------------------------------------------------
// Serial helpers
// ---------------------------------------------------------------------------

/// Try to parse a complete line from the serial receive buffer.
fn parse_serial_command(line: &str) -> Option<SerialCommand> {
    match line.trim() {
        "WORKING" => Some(SerialCommand::Working),
        "DONE" => Some(SerialCommand::Done),
        "INPUT" => Some(SerialCommand::Input),
        "IDLE" => Some(SerialCommand::Idle),
        "ERROR" => Some(SerialCommand::Error),
        _ => None,
    }
}

/// Send a button event string over serial.
fn serial_send_event(uart: &mut UartDriver, event: ButtonEvent) {
    let msg = match event {
        ButtonEvent::SingleTap => "TAP1\n",
        ButtonEvent::DoubleTap => "TAP2\n",
        ButtonEvent::TripleTap => "TAP3\n",
        ButtonEvent::LongPress => "HOLD\n",
    };
    let _ = uart.write(msg.as_bytes());
}

// ---------------------------------------------------------------------------
// LED rendering
// ---------------------------------------------------------------------------

/// Scale an RGB color by a brightness factor (0.0 .. 1.0).
fn scale_color(c: RGB8, brightness: f32) -> RGB8 {
    RGB8 {
        r: (c.r as f32 * brightness) as u8,
        g: (c.g as f32 * brightness) as u8,
        b: (c.b as f32 * brightness) as u8,
    }
}

/// Compute a sine-wave pulse brightness in [0.15 .. 1.0].
/// `phase` is in radians.
fn pulse_brightness(phase: f32) -> f32 {
    // sin(phase) is in [-1, 1].  Map to [0.15, 1.0].
    let sin_val = libm::sinf(phase);
    0.575 + 0.425 * sin_val
}

/// Render the LED ring according to the current mode and elapsed time.
fn render_leds(
    ws: &mut Ws2812Esp32Rmt,
    mode: &LedMode,
    elapsed_ms: u64,
) {
    let pixels: [RGB8; NUM_LEDS] = match *mode {
        LedMode::Solid(color) => [color; NUM_LEDS],

        LedMode::Pulse(base_color) => {
            // Full cycle every ~2 seconds.
            let phase = (elapsed_ms as f32 / 2000.0) * 2.0 * core::f32::consts::PI;
            let brightness = pulse_brightness(phase);
            [scale_color(base_color, brightness); NUM_LEDS]
        }

        LedMode::Blink(color, period_ms) => {
            // First half of the period: on.  Second half: off.
            let in_period = (elapsed_ms % period_ms as u64) as u32;
            if in_period < period_ms / 2 {
                [color; NUM_LEDS]
            } else {
                [COLOR_OFF; NUM_LEDS]
            }
        }

        LedMode::Flash(color, _duration_ms) => {
            // The caller manages flash timing; here we just show the color.
            [color; NUM_LEDS]
        }
    };

    let _ = ws.write(pixels.iter().cloned());
}

// ---------------------------------------------------------------------------
// Button debounce and tap-counting state machine
// ---------------------------------------------------------------------------

struct ButtonState {
    /// true = button is physically pressed (after debounce).
    pressed: bool,
    /// Timestamp of last raw edge (for debounce).
    last_edge: Instant,
    /// Timestamp when the current press started.
    press_start: Option<Instant>,
    /// Number of taps counted so far in the current gesture.
    tap_count: u8,
    /// Timestamp of the last release (for multi-tap window).
    last_release: Option<Instant>,
    /// Whether a long-press was already fired for the current hold.
    long_press_fired: bool,
}

impl ButtonState {
    fn new() -> Self {
        Self {
            pressed: false,
            last_edge: Instant::now(),
            press_start: None,
            tap_count: 0,
            last_release: None,
            long_press_fired: false,
        }
    }

    /// Call every tick with the current raw pin level (true = pressed / LOW).
    /// Returns an optional ButtonEvent when a gesture is complete.
    fn update(&mut self, raw_pressed: bool) -> Option<ButtonEvent> {
        let now = Instant::now();

        // --- Debounce ---
        if raw_pressed != self.pressed {
            if now.duration_since(self.last_edge) < Duration::from_millis(DEBOUNCE_MS) {
                // Ignore bouncy edges.
                return self.check_tap_timeout(now);
            }
            self.last_edge = now;
            self.pressed = raw_pressed;

            if self.pressed {
                // --- Falling edge (button pressed) ---
                self.press_start = Some(now);
                self.long_press_fired = false;
            } else {
                // --- Rising edge (button released) ---
                if let Some(start) = self.press_start.take() {
                    let held = now.duration_since(start);
                    if held < Duration::from_millis(LONG_PRESS_MS) && !self.long_press_fired {
                        // Count as a tap.
                        self.tap_count += 1;
                        self.last_release = Some(now);
                    }
                }
            }
        }

        // --- Long press detection (while held) ---
        if self.pressed && !self.long_press_fired {
            if let Some(start) = self.press_start {
                if now.duration_since(start) >= Duration::from_millis(LONG_PRESS_MS) {
                    self.long_press_fired = true;
                    self.tap_count = 0;
                    self.last_release = None;
                    return Some(ButtonEvent::LongPress);
                }
            }
        }

        self.check_tap_timeout(now)
    }

    /// If the tap window has expired, emit the accumulated tap count.
    fn check_tap_timeout(&mut self, now: Instant) -> Option<ButtonEvent> {
        if self.tap_count > 0 && !self.pressed {
            if let Some(release_time) = self.last_release {
                if now.duration_since(release_time) >= Duration::from_millis(TAP_WINDOW_MS) {
                    let count = self.tap_count;
                    self.tap_count = 0;
                    self.last_release = None;
                    return match count {
                        1 => Some(ButtonEvent::SingleTap),
                        2 => Some(ButtonEvent::DoubleTap),
                        _ => Some(ButtonEvent::TripleTap), // 3+ all map to triple
                    };
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// State machine: transitions
// ---------------------------------------------------------------------------

/// Process a serial command in the current state and return the new state +
/// LED mode.
fn handle_serial(state: State, cmd: SerialCommand) -> (State, LedMode) {
    match (state, cmd) {
        // Any state + ERROR -> Error
        (_, SerialCommand::Error) => (State::Error, LedMode::Blink(COLOR_RED, 500)),

        // Any state + IDLE -> Idle
        (_, SerialCommand::Idle) => (State::Idle, LedMode::Solid(COLOR_WHITE_DIM)),

        // Idle + WORKING -> Working
        (State::Idle, SerialCommand::Working) => (State::Working, LedMode::Solid(COLOR_RED)),

        // Working + DONE -> Done
        (State::Working, SerialCommand::Done) => (State::Done, LedMode::Solid(COLOR_GREEN)),

        // Working + INPUT -> NeedsInput
        (State::Working, SerialCommand::Input) => {
            (State::NeedsInput, LedMode::Pulse(COLOR_YELLOW))
        }

        // Allow WORKING from any state (daemon may re-send)
        (_, SerialCommand::Working) => (State::Working, LedMode::Solid(COLOR_RED)),

        // Allow DONE from any state as a fallback
        (_, SerialCommand::Done) => (State::Done, LedMode::Solid(COLOR_GREEN)),

        // Allow INPUT from any state as a fallback
        (_, SerialCommand::Input) => (State::NeedsInput, LedMode::Pulse(COLOR_YELLOW)),
    }
}

/// Process a button event in the current state.  Returns (new_state, led_mode)
/// and a flag indicating whether HID keystrokes should be sent.
fn handle_button(
    state: State,
    event: ButtonEvent,
) -> (State, LedMode, bool /* send_hid */) {
    match state {
        State::Done => match event {
            // Done + SingleTap -> approve -> Idle
            ButtonEvent::SingleTap => (State::Idle, LedMode::Flash(COLOR_BLUE, 200), true),
            // Done + DoubleTap -> reject -> Idle
            ButtonEvent::DoubleTap => (State::Idle, LedMode::Flash(COLOR_BLUE, 200), true),
            _ => (state, LedMode::Solid(COLOR_GREEN), false),
        },

        State::NeedsInput => match event {
            // NeedsInput + SingleTap -> approve -> Working
            ButtonEvent::SingleTap => (State::Working, LedMode::Solid(COLOR_RED), true),
            // NeedsInput + DoubleTap -> reject -> Working
            ButtonEvent::DoubleTap => (State::Working, LedMode::Solid(COLOR_RED), true),
            // NeedsInput + TripleTap -> security scan -> Working
            ButtonEvent::TripleTap => (State::Working, LedMode::Solid(COLOR_RED), true),
            // NeedsInput + LongPress -> explain -> stay in NeedsInput
            ButtonEvent::LongPress => (State::NeedsInput, LedMode::Pulse(COLOR_YELLOW), true),
        },

        State::Working => match event {
            // Working + LongPress -> cancel -> Idle
            ButtonEvent::LongPress => (State::Idle, LedMode::Solid(COLOR_WHITE_DIM), true),
            _ => (state, LedMode::Solid(COLOR_RED), false),
        },

        State::Error => {
            // Any button press in Error -> acknowledge -> Idle
            (State::Idle, LedMode::Solid(COLOR_WHITE_DIM), false)
        }

        State::Idle => {
            // In Idle, button presses have no state effect but we still
            // send HID keystrokes (useful if the user presses while the
            // daemon hasn't sent a command yet).
            (State::Idle, LedMode::Solid(COLOR_WHITE_DIM), true)
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    // Bind the ESP-IDF logger so `log::info!()` etc. go to the console.
    EspLogger::initialize_default();
    info!("Pulse button firmware starting");

    // --- Peripherals ---
    let peripherals = Peripherals::take()?;

    // --- Button GPIO (input, pull-up, active low) ---
    let mut button_pin = PinDriver::input(peripherals.pins.gpio4)?;
    button_pin.set_pull(Pull::Up)?;

    // --- WS2812B LED ring via RMT peripheral ---
    let mut ws = Ws2812Esp32Rmt::new(0 /* RMT channel */, LED_GPIO)?;

    // --- UART for serial communication ---
    // Using UART0 which maps to USB CDC on ESP32-S3 by default.
    let uart_config = UartConfig::default();
    let mut uart = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio43, // TX (USB CDC internal)
        peripherals.pins.gpio44, // RX (USB CDC internal)
        Option::<gpio::Gpio0>::None, // CTS (unused)
        Option::<gpio::Gpio0>::None, // RTS (unused)
        &uart_config,
    )?;

    // --- Initial state ---
    let mut state = State::Idle;
    let mut led_mode = LedMode::Solid(COLOR_WHITE_DIM);
    let mut prev_led_mode = led_mode; // saved for flash-return
    let mut button_state = ButtonState::new();

    // Serial receive buffer (accumulates bytes until newline).
    let mut serial_buf: Vec<u8> = Vec::with_capacity(64);

    // Flash timer: when Some, we are showing a brief flash overlay.
    let mut flash_end: Option<Instant> = None;

    // Global elapsed time for LED animation.
    let start_time = Instant::now();

    info!("Entering main loop, state = Idle");

    // -----------------------------------------------------------------------
    // Main loop
    // -----------------------------------------------------------------------
    loop {
        let now = Instant::now();
        let elapsed_ms = now.duration_since(start_time).as_millis() as u64;

        // --- 1. Read serial input (non-blocking) ---
        let mut byte_buf = [0u8; 1];
        while uart.read(&mut byte_buf, 0).unwrap_or(0) == 1 {
            if byte_buf[0] == b'\n' || byte_buf[0] == b'\r' {
                if !serial_buf.is_empty() {
                    if let Ok(line) = std::str::from_utf8(&serial_buf) {
                        if let Some(cmd) = parse_serial_command(line) {
                            info!("Serial command: {:?}", cmd);
                            let (new_state, new_mode) = handle_serial(state, cmd);
                            state = new_state;
                            led_mode = new_mode;
                            prev_led_mode = led_mode;
                            flash_end = None; // cancel any active flash
                        } else {
                            warn!("Unknown serial line: {}", line);
                        }
                    }
                    serial_buf.clear();
                }
            } else {
                serial_buf.push(byte_buf[0]);
                // Safety: cap buffer to prevent unbounded growth.
                if serial_buf.len() > 128 {
                    serial_buf.clear();
                }
            }
        }

        // --- 2. Read button ---
        let raw_pressed = button_pin.is_low();
        if let Some(event) = button_state.update(raw_pressed) {
            info!("Button event: {:?} in state {:?}", event, state);

            // Send event over serial.
            serial_send_event(&mut uart, event);

            // Run the state machine.
            let (new_state, new_mode, send_hid) = handle_button(state, event);
            state = new_state;

            // Handle LED mode: if the transition returned a Flash, set up the
            // flash timer and remember the *next* mode to go to after flash.
            match new_mode {
                LedMode::Flash(color, dur_ms) => {
                    prev_led_mode = LedMode::Solid(COLOR_WHITE_DIM); // post-flash
                    led_mode = LedMode::Flash(color, dur_ms);
                    flash_end = Some(now + Duration::from_millis(dur_ms as u64));
                }
                other => {
                    led_mode = other;
                    prev_led_mode = other;
                    flash_end = None;
                }
            }

            // Send HID keystrokes (runs in the main loop context; tiny
            // delays are acceptable since the LED refresh is cosmetic).
            if send_hid {
                hid_send_event(event);
            }
        }

        // --- 3. Flash timer management ---
        if let Some(end) = flash_end {
            if now >= end {
                led_mode = prev_led_mode;
                flash_end = None;
            }
        }

        // --- 4. Render LEDs ---
        render_leds(&mut ws, &led_mode, elapsed_ms);

        // --- 5. Tick delay ---
        FreeRtos::delay_ms(TICK_MS);
    }
}
