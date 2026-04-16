# Pulse Button -- ESP32-S3 Firmware

Physical approve/reject button for Claude Code. Connects over USB to the host
machine running the Pulse daemon, sends HID keystrokes and serial events.

## Parts List

| Part                        | Qty | Notes                                    |
|-----------------------------|-----|------------------------------------------|
| ESP32-S3 Super Mini         | 1   | Must be the S3 variant (native USB OTG)  |
| WS2812B LED Ring (16 LEDs)  | 1   | NeoPixel-compatible, 5 V logic           |
| Cherry MX switch (any)      | 1   | Or any compatible mechanical key switch  |
| USB-C cable                 | 1   | Data-capable (not charge-only)           |
| 3D-printed enclosure        | 1   | Optional; STL files in ../enclosure/     |
| Hookup wire                 | --  | 24-26 AWG stranded recommended           |

## Pin Assignments

| Signal         | GPIO | Direction | Notes                          |
|----------------|------|-----------|--------------------------------|
| WS2812B Data   | 8    | Output    | 3.3 V logic OK for short runs  |
| Button         | 4    | Input     | Internal pull-up, active LOW   |
| USB D+ / D-    | 19/20| Bidir     | Native USB OTG (S3 built-in)   |

## Wiring Diagram

```
                 ESP32-S3 Super Mini
                 +-----------------+
                 |                 |
    USB-C <----->|  USB  (GPIO19/20)  <--- to host PC
                 |                 |
                 |  GPIO 8  ------+-------> WS2812B DIN
                 |                 |
                 |  GPIO 4  ------+-------> Cherry MX pin 1
                 |                 |
                 |  GND     ------+---+--> Cherry MX pin 2
                 |                 |  +--> WS2812B GND
                 |                 |
                 |  5V (VBUS) ----+-------> WS2812B VCC
                 |                 |
                 +-----------------+

  Cherry MX switch (active low):
      pin 1 ----> GPIO 4 (has internal pull-up)
      pin 2 ----> GND

  WS2812B ring:
      DIN   ----> GPIO 8
      VCC   ----> 5V (from USB VBUS)
      GND   ----> GND
```

## Build Instructions

### 1. Install the esp-rs toolchain

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install espup (ESP Rust toolchain manager)
cargo install espup
espup install          # installs the xtensa Rust fork + ESP-IDF

# Source the environment (add to your shell profile)
. $HOME/export-esp.sh

# Install cargo-espflash for flashing
cargo install cargo-espflash
```

### 2. Build

```bash
cd firmware/
cargo build --release
```

### 3. Flash

```bash
# With the ESP32-S3 connected via USB:
cargo espflash flash --release --monitor

# Or just monitor serial output:
cargo espflash monitor
```

### 4. Verify

Once flashed, the LED ring should light dim white (Idle state). Press the
button to confirm it responds. Connect the Pulse daemon on the host to begin
receiving state commands over serial.

## Protocol

### Host -> Device (serial lines)

| Command     | Effect                         |
|-------------|--------------------------------|
| `WORKING\n` | LED solid red, Working state   |
| `DONE\n`    | LED solid green, Done state    |
| `INPUT\n`   | LED pulsing yellow, NeedsInput |
| `IDLE\n`    | LED dim white, Idle state      |
| `ERROR\n`   | LED blinking red, Error state  |

### Device -> Host (serial lines)

| Event    | Meaning              |
|----------|----------------------|
| `TAP1\n` | Single tap detected  |
| `TAP2\n` | Double tap detected  |
| `TAP3\n` | Triple tap detected  |
| `HOLD\n` | Long press (>2 sec)  |

### HID Keystrokes (sent as USB keyboard)

| Event      | Keystroke         |
|------------|-------------------|
| SingleTap  | `y` + Enter       |
| DoubleTap  | `n` + Enter       |
| TripleTap  | `/security` + Enter |
| LongPress  | `/explain` + Enter  |
