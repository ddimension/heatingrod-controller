//! Hardware abstraction — traits + mocks.
//!
//! Generalizes Python's `--mock-dac` to a full offline mode: the controller
//! logic depends only on these traits, so the entire decision machinery runs
//! with mocks (Phase 3) and the real drivers plug in behind the same
//! interfaces (Phase 4: I2C DAC, DS100 Modbus, SOREL CAN, 1-Wire FFI,
//! ESPHome clients).

pub mod canbus;
pub mod dac;
pub mod ds100;
pub mod onewire;
pub mod slcan;
pub mod traits;

pub use canbus::{AirInPipeDetector, DecodedMessage, ScbiId, SorelDecoder, Stats};
pub use dac::{I2cBus, LinuxI2cBus, MockI2cBus, RealDac};
pub use ds100::{
    baud_to_index, crc16, index_to_baud, DS100DeviceInfo, DS100Poller, DS100Reading,
    DS100Serial, LinuxSerialPort, MockSerial, SerialTransport,
};
pub use onewire::{OneWirePoller, OneWireSensor};
pub use slcan::SlcanTransport;
pub use traits::{system_now, ControllerIo, Dac, MockDac, MockEnv, SensorRole};
