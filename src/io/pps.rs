// SPDX-License-Identifier: BSD-3-Clause
use embassy_time::{Duration, Instant, Ticker, with_timeout};

pub use crate::driver::pps::{PpsError, PpsRunningMode};
use crate::driver::pps::PpsDriver;
use crate::io::shared_i2c::SharedI2cBus;

pub struct PpsReadings {
    pub voltage: f32,
    pub current: f32,
    pub temperature: f32,
    pub input_voltage: f32,
    pub running_mode: PpsRunningMode,
}

pub struct PpsSetpoint {
    pub current_limit: Option<f32>,
    pub voltage_limit: Option<f32>,
    pub enabled: Option<bool>,
}

pub struct PpsResources {
    pub i2c: &'static SharedI2cBus,
}

const PPS_LOOP_TIME_MS: u64 = 500;

async fn read_pps(pps: &mut PpsDriver) -> Result<PpsReadings, PpsError> {
    let voltage = pps.get_voltage().await?;
    let current = pps.get_current().await?;
    let temperature = pps.get_temperature().await?;
    let input_voltage = pps.get_input_voltage().await?;
    let running_mode = pps.get_running_mode().await?;
    Ok(PpsReadings { voltage, current, temperature, input_voltage, running_mode })
}

async fn write_pps(pps: &mut PpsDriver, setpoint: &PpsSetpoint) -> Result<(), PpsError> {
    debug!(
        "write_pps: cl: {:?} vl: {:?} enabled: {:?}",
        setpoint.current_limit, setpoint.voltage_limit, setpoint.enabled
    );
    if let Some(cl) = setpoint.current_limit {
        pps.set_current(cl).await?;
    }
    if let Some(vl) = setpoint.voltage_limit {
        pps.set_voltage(vl).await?;
    }
    match setpoint.enabled {
        Some(en) => {
            pps.enable(en).await?;
        }
        None => (),
    }
    Ok(())
}

async fn poll_pps(
    pps: &mut PpsDriver,
    on_read: fn(&PpsReadings),
    get_setpoint: fn() -> PpsSetpoint,
) -> Result<(), PpsError> {
    let setpoint = get_setpoint();
    write_pps(pps, &setpoint).await?;
    let readings = read_pps(pps).await?;
    on_read(&readings);
    Ok(())
}

/// Full PPS loop: 500ms ticker, 1500ms timeout, error counting (break after 10).
pub async fn pps_loop(
    resources: PpsResources,
    on_read: fn(&PpsReadings),
    get_setpoint: fn() -> PpsSetpoint,
) {
    let mut pps = PpsDriver::new(resources.i2c, 0x35);
    pps.enable(false).await.ok();

    let mut ticker = Ticker::every(Duration::from_millis(PPS_LOOP_TIME_MS));
    let mut error_count = 0;
    loop {
        let loop_start = Instant::now();
        let timeout_result =
            with_timeout(Duration::from_millis(PPS_LOOP_TIME_MS * 3), poll_pps(&mut pps, on_read, get_setpoint))
                .await;
        match timeout_result {
            Ok(poll_result) => match poll_result {
                Ok(_) => {
                    error_count = 0;
                }
                Err(err) => {
                    warn!("PPS error: {}", err);
                    error_count += 1;
                    if error_count > 10 {
                        error!("stopping PPS task after 10 consecutive errors");
                        break;
                    }
                }
            },
            Err(err) => {
                error!("timeout in io i2c loop: {:?}", err);
                ticker.reset_at(Instant::now() - Duration::from_millis(PPS_LOOP_TIME_MS));
            }
        }
        let loop_time = loop_start.elapsed();
        debug!("io loop time: {:?} ms", loop_time.as_millis());
        ticker.next().await;
    }
}
