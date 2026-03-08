// SPDX-License-Identifier: BSD-3-Clause
//! Programmable Power Supply (PPS) module driver (I2C 0x35).
//!
//! Custom I2C command/response protocol with single-byte command register
//! followed by 1–4 byte payload. Voltage/current values are IEEE 754 f32 LE.
//!
//! Read register map:
//!   0x00  ModuleId         2 bytes, u16 LE
//!   0x05  RunningMode      1 byte: 0=Off, 1=Voltage, 2=Current, 3=Unknown
//!   0x07  DataFlag         1 byte
//!   0x08  ReadbackVoltage  4 bytes, f32 LE (volts)
//!   0x0C  ReadbackCurrent  4 bytes, f32 LE (amps)
//!   0x10  Temperature      4 bytes, f32 LE (°C)
//!   0x14  InputVoltage     4 bytes, f32 LE (volts)
//!   0x50  Address          1 byte
//!   0x52  UID word 0       4 bytes
//!   0x56  UID word 1       4 bytes
//!   0x5A  UID word 2       4 bytes
//!
//! Write register map:
//!   0x04  Enable           1 byte: 0=disable, 1=enable
//!   0x18  SetVoltage       4 bytes, f32 LE (volts)
//!   0x1C  SetCurrent       4 bytes, f32 LE (amps)
//!
//! Hardware: M5Stack PPS module (SKU: U136)
//! Ref: <https://docs.m5stack.com/en/unit/PPS>
use esp_hal::{Async, i2c::master::I2c};
use thiserror_no_std::Error;

use crate::io::shared_i2c::SharedI2cBus;

#[repr(u8)]
#[derive(Debug, Default, Clone, Copy)]
pub enum PpsRunningMode {
    Off = 0,
    Voltage = 1,
    Current = 2,
    #[default]
    Unknown = 3,
}

impl PpsRunningMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Off),
            1 => Some(Self::Voltage),
            2 => Some(Self::Current),
            3 => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum PpsError {
    #[error("unknown error")]
    Unknown,

    #[error("Failed to read from PPS module")]
    ReadError,

    #[error("Command not implemented")]
    UnsupportedCommand,

    #[error("I2C master error: {0:?}")]
    I2cMasterError(#[from] esp_hal::i2c::master::Error),
}

type I2cType = I2c<'static, Async>;

#[allow(dead_code)]
enum ReadCommand {
    ModuleId,
    GetRunningMode,
    GetDataFlag,
    ReadbackVoltage,
    ReadbackCurrent,
    GetTemperature,
    GetInputVoltage,
    GetAddress,
    PsuUidW0,
    PsuUidW1,
    PsuUidW2,
}

pub enum ReadResult {
    ModuleId(u16),
    RunningMode(PpsRunningMode),
    ReadbackVoltage(f32),
    ReadbackCurrent(f32),
    Temperature(f32),
    InputVoltage(f32),
}

impl ReadCommand {
    fn get_read_command(&self) -> (u8, usize) {
        match self {
            ReadCommand::ModuleId => (0x0, 2),
            ReadCommand::GetRunningMode => (0x05, 1),
            ReadCommand::GetDataFlag => (0x07, 1),
            ReadCommand::ReadbackVoltage => (0x08, 4),
            ReadCommand::ReadbackCurrent => (0x0c, 4),
            ReadCommand::GetTemperature => (0x10, 4),
            ReadCommand::GetInputVoltage => (0x14, 4),
            ReadCommand::GetAddress => (0x50, 1),
            ReadCommand::PsuUidW0 => (0x52, 4),
            ReadCommand::PsuUidW1 => (0x56, 4),
            ReadCommand::PsuUidW2 => (0x5a, 4),
        }
    }

    fn evaluate_result(&self, buffer: &[u8; 4]) -> Result<ReadResult, PpsError> {
        match self {
            ReadCommand::ModuleId => Ok(ReadResult::ModuleId(
                (buffer[1] as u16) << 8 | buffer[0] as u16,
            )),
            ReadCommand::GetRunningMode => Ok(ReadResult::RunningMode(
                PpsRunningMode::from_u8(buffer[0]).ok_or(PpsError::Unknown)?,
            )),
            ReadCommand::ReadbackVoltage => {
                Ok(ReadResult::ReadbackVoltage(f32::from_le_bytes(*buffer)))
            }
            ReadCommand::ReadbackCurrent => {
                Ok(ReadResult::ReadbackCurrent(f32::from_le_bytes(*buffer)))
            }
            ReadCommand::GetTemperature => Ok(ReadResult::Temperature(f32::from_le_bytes(*buffer))),
            ReadCommand::GetInputVoltage => {
                Ok(ReadResult::InputVoltage(f32::from_le_bytes(*buffer)))
            }
            _ => Err(PpsError::UnsupportedCommand),
        }
    }

    pub async fn receive_async(
        self,
        i2c: &mut I2cType,
        address: u8,
    ) -> Result<ReadResult, PpsError> {
        let (cmd, bytes_to_read) = self.get_read_command();
        let mut buffer = [0_u8; 4];
        i2c.write_read_async(address, &[cmd], &mut buffer[..bytes_to_read])
            .await?;
        self.evaluate_result(&buffer)
    }
}

#[derive(Debug)]
enum WriteCommand {
    ModuleEnable(bool),
    SetVoltage(f32),
    SetCurrent(f32),
}

impl WriteCommand {
    fn get_write_command(&self, buffer: &mut [u8; 5]) -> usize {
        match self {
            WriteCommand::ModuleEnable(enable) => {
                buffer[0] = 0x04;
                buffer[1] = *enable as u8;
                2
            }
            WriteCommand::SetVoltage(voltage) => {
                buffer[0] = 0x18;
                buffer[1..].copy_from_slice(voltage.to_le_bytes().as_slice());
                5
            }
            WriteCommand::SetCurrent(current) => {
                buffer[0] = 0x1c;
                buffer[1..].copy_from_slice(current.to_le_bytes().as_slice());
                5
            }
        }
    }

    pub async fn send_async(self, i2c: &mut I2cType, address: u8) -> Result<(), PpsError> {
        debug!("send: {:?} to address 0x{:x}", self, address);
        let mut buffer = [0x0_u8; 5];
        let bytes_to_write = self.get_write_command(&mut buffer);
        i2c.write_async(address, &buffer[..bytes_to_write]).await?;
        Ok(())
    }
}

pub struct PpsDriver {
    i2c: &'static SharedI2cBus,
    address: u8,
}

impl PpsDriver {
    pub fn new(i2c: &'static SharedI2cBus, address: u8) -> Self {
        Self { i2c, address }
    }

    pub async fn set_current(&mut self, current: f32) -> Result<&mut Self, PpsError> {
        let cmd = WriteCommand::SetCurrent(current);
        debug!("set current {}, sending command: {:?}", current, cmd);
        let mut bus = self.i2c.lock().await;
        cmd.send_async(&mut *bus, self.address).await?;
        Ok(self)
    }

    pub async fn set_voltage(&mut self, voltage: f32) -> Result<&mut Self, PpsError> {
        let cmd = WriteCommand::SetVoltage(voltage);
        let mut bus = self.i2c.lock().await;
        cmd.send_async(&mut *bus, self.address).await?;
        Ok(self)
    }

    pub async fn enable(&mut self, enabled: bool) -> Result<&mut Self, PpsError> {
        let cmd = WriteCommand::ModuleEnable(enabled);
        let mut bus = self.i2c.lock().await;
        cmd.send_async(&mut *bus, self.address).await?;
        Ok(self)
    }

    pub async fn get_running_mode(&mut self) -> Result<PpsRunningMode, PpsError> {
        let mut bus = self.i2c.lock().await;
        match ReadCommand::GetRunningMode
            .receive_async(&mut *bus, self.address)
            .await?
        {
            ReadResult::RunningMode(mode) => Ok(mode),
            _ => Err(PpsError::ReadError),
        }
    }

    pub async fn get_voltage(&mut self) -> Result<f32, PpsError> {
        let mut bus = self.i2c.lock().await;
        match ReadCommand::ReadbackVoltage
            .receive_async(&mut *bus, self.address)
            .await?
        {
            ReadResult::ReadbackVoltage(voltage) => Ok(voltage),
            _ => Err(PpsError::ReadError),
        }
    }

    pub async fn get_current(&mut self) -> Result<f32, PpsError> {
        let mut bus = self.i2c.lock().await;
        match ReadCommand::ReadbackCurrent
            .receive_async(&mut *bus, self.address)
            .await?
        {
            ReadResult::ReadbackCurrent(current) => Ok(current),
            _ => Err(PpsError::ReadError),
        }
    }

    pub async fn get_temperature(&mut self) -> Result<f32, PpsError> {
        let mut bus = self.i2c.lock().await;
        match ReadCommand::GetTemperature
            .receive_async(&mut *bus, self.address)
            .await?
        {
            ReadResult::Temperature(temp) => Ok(temp),
            _ => Err(PpsError::ReadError),
        }
    }

    pub async fn get_input_voltage(&mut self) -> Result<f32, PpsError> {
        let mut bus = self.i2c.lock().await;
        match ReadCommand::GetInputVoltage
            .receive_async(&mut *bus, self.address)
            .await?
        {
            ReadResult::InputVoltage(voltage) => Ok(voltage),
            _ => Err(PpsError::ReadError),
        }
    }

    #[allow(dead_code)]
    pub async fn get_module_id(&mut self) -> Result<u16, PpsError> {
        let mut bus = self.i2c.lock().await;
        match ReadCommand::ModuleId
            .receive_async(&mut *bus, self.address)
            .await?
        {
            ReadResult::ModuleId(id) => Ok(id),
            _ => Err(PpsError::ReadError),
        }
    }
}
