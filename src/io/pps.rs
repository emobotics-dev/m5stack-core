// SPDX-License-Identifier: MIT OR Apache-2.0
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};

use crate::driver::pps::PpsDriver;
pub use crate::driver::pps::{PpsError, PpsRunningMode};
use crate::io::shared_i2c::SharedI2cBus;

pub struct PpsReadings {
    pub voltage: f32,
    pub current: f32,
    pub temperature: f32,
    pub input_voltage: f32,
    pub running_mode: PpsRunningMode,
    /// Readings rejected as invalid since boot. Rides along on the next good
    /// batch so a consumer can log it: rejections are otherwise visible only
    /// on the console, which is not attached in the field.
    pub rejected: u32,
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

/// The first read can legitimately miss: nothing sequences our first
/// transaction against the module's own power-up.
const PROBE_ATTEMPTS: u32 = 3;

async fn read_pps(pps: &mut PpsDriver) -> Result<PpsReadings, PpsError> {
    let voltage = pps.get_voltage().await?;
    let current = pps.get_current().await?;
    let temperature = pps.get_temperature().await?;
    let input_voltage = pps.get_input_voltage().await?;
    let running_mode = pps.get_running_mode().await?;
    Ok(PpsReadings {
        voltage,
        current,
        temperature,
        input_voltage,
        running_mode,
        rejected: 0, // stamped by the caller, which owns the count
    })
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
    rejected: u32,
) -> Result<(), PpsError> {
    let setpoint = get_setpoint();
    write_pps(pps, &setpoint).await?;
    let mut readings = read_pps(pps).await?;
    readings.rejected = rejected;
    on_read(&readings);
    Ok(())
}

/// Full PPS loop: 500ms ticker, 1500ms timeout, and a consecutive-error budget
/// the task stops itself on.
///
/// On a bench with no module the probe NACKs to exhaustion, the loop spends the
/// budget, and the task returns. **Not a failure** — the budget exists so a
/// wedged PPS cannot keep the bus hot forever; other tasks keep running.
/// `examples/probe_pps.rs` drives exactly this.
///
/// Counting the log lines: exhaustion reports at `error!` (the retry guard is
/// `attempt < PROBE_ATTEMPTS`), and the budget ends one past its threshold
/// because `error_count` is tested after the increment.
pub async fn pps_loop(
    resources: PpsResources,
    on_read: fn(&PpsReadings),
    get_setpoint: fn() -> PpsSetpoint,
) {
    let mut pps = PpsDriver::new(resources.i2c, 0x35);
    pps.enable(false).await.ok();

    // Not fatal on failure: the loop below reaches the same verdict via the
    // normal error path. This just says why in one line instead of ten NACKs.
    for attempt in 1..=PROBE_ATTEMPTS {
        match pps.probe().await {
            Ok(_) => break,
            Err(err) if attempt < PROBE_ATTEMPTS => {
                warn!(
                    "PPS probe attempt {}/{} failed: {}",
                    attempt, PROBE_ATTEMPTS, err
                );
                Timer::after(Duration::from_millis(PPS_LOOP_TIME_MS)).await;
            }
            Err(err) => {
                error!(
                    "PPS did not identify itself in {} attempts: {}",
                    PROBE_ATTEMPTS, err
                );
            }
        }
    }

    let mut ticker = Ticker::every(Duration::from_millis(PPS_LOOP_TIME_MS));
    let mut error_count = 0;
    let mut rejected: u32 = 0;
    loop {
        let loop_start = Instant::now();
        let timeout_result = with_timeout(
            Duration::from_millis(PPS_LOOP_TIME_MS * 3),
            poll_pps(&mut pps, on_read, get_setpoint, rejected),
        )
        .await;
        match timeout_result {
            Ok(poll_result) => match poll_result {
                Ok(_) => {
                    error_count = 0;
                }
                // Off the budget: spending it here would turn a rare rejected
                // sample into a permanently dead PPS task.
                Err(err) if err.is_transient() => {
                    rejected = rejected.saturating_add(1);
                    warn!("PPS reading rejected ({} total): {}", rejected, err);
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
