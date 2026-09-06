//! dac-safe — raw I2C failsafe for systemd ExecStopPost.
//!
//! Replaces the Python one-liner from heatingrod-controller-v2.service:
//!   b.write_word_data(0x5f, 0x02, 0); b.write_word_data(0x5f, 0x04, 39312)
//! Same kernel SMBus ioctls (via the i2cdev crate) → identical wire behavior.
//!
//! Semantics: CH0 (Heizstab) → 0V; CH1 (ProControl HK1 setpoint) → 6V = 60°C
//! — the boiler keeps running freely, never blocked by a dead controller.

use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;

const DAC_ADDR: u16 = 0x5f;
const REG_RANGE: u8 = 0x01;
const REG_CH0: u8 = 0x02;
const REG_CH1: u8 = 0x04;
/// 6V = 60°C: (6000/10000 * 4095) << 4
const CH1_SAFE: u16 = 39312;

fn main() {
    let mut dev = match LinuxI2CDevice::new("/dev/i2c-1", DAC_ADDR) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("dac-safe: cannot open /dev/i2c-1 @ 0x{DAC_ADDR:02x}: {e}");
            std::process::exit(1);
        }
    };
    // Range config (10V) is harmless to re-assert; CH0=0 and CH1=safe matter.
    if let Err(e) = dev.smbus_write_word_data(REG_RANGE, 0x11) {
        eprintln!("dac-safe: range write failed: {e}");
    }
    if let Err(e) = dev.smbus_write_word_data(REG_CH0, 0) {
        eprintln!("dac-safe: CH0 write failed: {e}");
        std::process::exit(1);
    }
    if let Err(e) = dev.smbus_write_word_data(REG_CH1, CH1_SAFE) {
        eprintln!("dac-safe: CH1 write failed: {e}");
        std::process::exit(1);
    }
    println!("DAC safe: CH0=0V CH1=6V(60C)");
}
